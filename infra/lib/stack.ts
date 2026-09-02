import * as cdk from "aws-cdk-lib";
import * as ec2 from "aws-cdk-lib/aws-ec2";
import * as ecr from "aws-cdk-lib/aws-ecr";
import * as ecs from "aws-cdk-lib/aws-ecs";
import * as elbv2 from "aws-cdk-lib/aws-elasticloadbalancingv2";
import * as iam from "aws-cdk-lib/aws-iam";
import * as logs from "aws-cdk-lib/aws-logs";
import * as secretsmanager from "aws-cdk-lib/aws-secretsmanager";
import * as wafv2 from "aws-cdk-lib/aws-wafv2";
import { Construct } from "constructs";

export interface ARvmStackProps extends cdk.StackProps {
  readonly certificateArn: string;
  readonly secretName: string;
  readonly region: string;
  readonly domainName: string;
  readonly imageTag: string;
  readonly openRouterModel: string;
}

export class ARvmStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: ARvmStackProps) {
    super(scope, id, props);

    if (!props.certificateArn || !props.certificateArn.startsWith("arn:aws:acm:")) {
      throw new Error("certificateArn must be a valid ACM ARN (arn:aws:acm:REGION:ACCOUNT:certificate/...)");
    }
    if (!props.secretName || props.secretName.trim().length === 0) {
      throw new Error("secretName must be the name of an existing Secrets Manager secret");
    }

    const vpc = new ec2.Vpc(this, "Vpc", {
      natGateways: 1,
      availabilityZones: [`${props.region}a`, `${props.region}b`],
      subnetConfiguration: [
        { name: "Public", subnetType: ec2.SubnetType.PUBLIC, cidrMask: 24 },
        { name: "Private", subnetType: ec2.SubnetType.PRIVATE_WITH_EGRESS, cidrMask: 24 },
      ],
    });

    const repository = new ecr.Repository(this, "Repository", {
      repositoryName: "a-rust-vm",
      imageTagMutability: ecr.TagMutability.IMMUTABLE,
      imageScanOnPush: true,
      removalPolicy: cdk.RemovalPolicy.RETAIN,
    });
    repository.addLifecycleRule({
      maxImageCount: 10,
      rulePriority: 1,
      description: "keep last 10 images",
    });

    const logGroup = new logs.LogGroup(this, "LogGroup", {
      logGroupName: "/ecs/a-rust-vm",
      retention: logs.RetentionDays.ONE_MONTH,
      removalPolicy: cdk.RemovalPolicy.DESTROY,
    });

    const cluster = new ecs.Cluster(this, "Cluster", {
      vpc,
      containerInsights: true,
    });

    const albSg = new ec2.SecurityGroup(this, "AlbSg", {
      vpc,
      description: "ALB security group",
      allowAllOutbound: true,
    });
    albSg.addIngressRule(ec2.Peer.anyIpv4(), ec2.Port.tcp(80), "HTTP");
    albSg.addIngressRule(ec2.Peer.anyIpv4(), ec2.Port.tcp(443), "HTTPS");

    const taskSg = new ec2.SecurityGroup(this, "TaskSg", {
      vpc,
      description: "Fargate task security group",
      allowAllOutbound: false,
    });
    taskSg.addIngressRule(albSg, ec2.Port.tcp(8080), "ALB to task 8080");
    taskSg.addEgressRule(ec2.Peer.anyIpv4(), ec2.Port.tcp(443), "OpenRouter HTTPS egress");

    const executionRole = new iam.Role(this, "ExecutionRole", {
      assumedBy: new iam.ServicePrincipal("ecs-tasks.amazonaws.com"),
      description: "ECS execution role for ECR/logs/Secrets Manager only",
    });
    executionRole.addManagedPolicy(
      iam.ManagedPolicy.fromAwsManagedPolicyName("service-role/AmazonECSTaskExecutionRolePolicy")
    );

    const taskRole = new iam.Role(this, "TaskRole", {
      assumedBy: new iam.ServicePrincipal("ecs-tasks.amazonaws.com"),
      description: "ECS task role with no extra permissions",
    });

    const secret = secretsmanager.Secret.fromSecretNameV2(this, "OpenRouterSecret", props.secretName);
    secret.grantRead(executionRole);

    const taskDefinition = new ecs.FargateTaskDefinition(this, "TaskDef", {
      cpu: 512,
      memoryLimitMiB: 1024,
      runtimePlatform: {
        cpuArchitecture: ecs.CpuArchitecture.X86_64,
        operatingSystemFamily: ecs.OperatingSystemFamily.LINUX,
      },
      executionRole,
      taskRole,
    });

    taskDefinition.addContainer("app", {
      image: ecs.ContainerImage.fromEcrRepository(repository, props.imageTag),
      portMappings: [{ containerPort: 8080, protocol: ecs.Protocol.TCP }],
      logging: ecs.LogDrivers.awsLogs({ logGroup, streamPrefix: "a-rust-vm" }),
      environment: {
        OPENROUTER_MODEL: props.openRouterModel,
        A_RVM_ALLOWED_ORIGIN: `https://${props.domainName}`,
        A_RVM_SECURE_COOKIES: "true",
        A_RVM_WEB_PORT: "8080",
        A_RVM_BIND_ADDRESS: "0.0.0.0",
      },
      secrets: {
        OPENROUTER_API_KEY: ecs.Secret.fromSecretsManager(secret),
      },
      stopTimeout: cdk.Duration.seconds(30),
      readonlyRootFilesystem: true,
    });

    const alb = new elbv2.ApplicationLoadBalancer(this, "Alb", {
      vpc,
      internetFacing: true,
      securityGroup: albSg,
      vpcSubnets: { subnetType: ec2.SubnetType.PUBLIC },
    });

    const httpsListener = alb.addListener("HttpsListener", {
      port: 443,
      protocol: elbv2.ApplicationProtocol.HTTPS,
      certificates: [elbv2.ListenerCertificate.fromArn(props.certificateArn)],
    });

    alb.addListener("HttpListener", {
      port: 80,
      protocol: elbv2.ApplicationProtocol.HTTP,
      defaultAction: elbv2.ListenerAction.redirect({
        protocol: "HTTPS",
        port: "443",
        permanent: true,
      }),
    });

    const service = new ecs.FargateService(this, "Service", {
      cluster,
      taskDefinition,
      desiredCount: 1,
      assignPublicIp: false,
      vpcSubnets: { subnetType: ec2.SubnetType.PRIVATE_WITH_EGRESS },
      securityGroups: [taskSg],
      circuitBreaker: { rollback: true },
      healthCheckGracePeriod: cdk.Duration.seconds(60),
      minHealthyPercent: 100,
      maxHealthyPercent: 200,
    });

    const targetGroup = httpsListener.addTargets("EcsTargets", {
      port: 8080,
      protocol: elbv2.ApplicationProtocol.HTTP,
      targets: [service],
      healthCheck: {
        path: "/healthz",
        port: "traffic-port",
        healthyHttpCodes: "200",
        interval: cdk.Duration.seconds(30),
        timeout: cdk.Duration.seconds(5),
        healthyThresholdCount: 2,
        unhealthyThresholdCount: 3,
      },
      deregistrationDelay: cdk.Duration.seconds(30),
    });

    void targetGroup;

    const webAcl = new wafv2.CfnWebACL(this, "WebAcl", {
      scope: "REGIONAL",
      defaultAction: { allow: {} },
      visibilityConfig: {
        cloudWatchMetricsEnabled: true,
        metricName: "a-rust-vm-waf",
        sampledRequestsEnabled: true,
      },
      rules: [
        {
          name: "AWSManagedRulesCommonRuleSet",
          priority: 10,
          statement: {
            managedRuleGroupStatement: {
              vendorName: "AWS",
              name: "AWSManagedRulesCommonRuleSet",
            },
          },
          overrideAction: { none: {} },
          visibilityConfig: {
            cloudWatchMetricsEnabled: true,
            metricName: "common",
            sampledRequestsEnabled: true,
          },
        },
        {
          name: "RateLimitPerIp",
          priority: 20,
          statement: {
            rateBasedStatement: {
              limit: 500,
              aggregateKeyType: "IP",
            },
          },
          action: { block: {} },
          visibilityConfig: {
            cloudWatchMetricsEnabled: true,
            metricName: "rateLimit",
            sampledRequestsEnabled: true,
          },
        },
      ],
    });

    new wafv2.CfnWebACLAssociation(this, "WebAclAssociation", {
      resourceArn: alb.loadBalancerArn,
      webAclArn: webAcl.attrArn,
    });

    new cdk.CfnOutput(this, "AlbDnsName", { value: alb.loadBalancerDnsName });
    new cdk.CfnOutput(this, "EcrRepositoryUri", { value: repository.repositoryUri });
    new cdk.CfnOutput(this, "ServiceName", { value: service.serviceName });

    cdk.Tags.of(this).add("Project", "a-rust-vm");
  }
}
