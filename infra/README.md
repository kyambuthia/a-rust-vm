# A/RVM deployment — ECS/Fargate + ALB + WAF

Public hostname: `https://asiliano.online`

## Prerequisites

- Docker, Node 20+, AWS CLI v2, AWS CDK v2 (`npm install -g aws-cdk` or via `npx cdk`)
- An AWS account/region bootstrapped for CDK (`cdk bootstrap aws://ACCOUNT/REGION`)
- Permissions to create VPC, ECR, ECS, ALB, WAF, IAM, CloudWatch Logs
- An existing ACM certificate for `asiliano.online` in the deploy region (ALB requires a region-local ACM cert)
- An existing Secrets Manager secret containing only the OpenRouter API key as its plaintext secret value

Never paste real secrets into source, `cdk.json`, or `cdk context`.

## Create/import required external resources

**ACM certificate (replace placeholder values):**

```bash
aws acm request-certificate --domain-name asiliano.online \
  --validation-method DNS --region us-east-1
# or import:
aws acm import-certificate --certificate fileb://cert.pem \
  --private-key fileb://key.pem --certificate-chain fileb://chain.pem --region us-east-1
```

Note the returned `CertificateArn` like `arn:aws:acm:us-east-1:123456789012:certificate/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee` — the stack takes this as input and does not create certificates.

**Secrets Manager secret (replace placeholder):**

```bash
aws secretsmanager create-secret \
  --name prod/openrouter/api-key \
  --secret-string 'sk-or-v1-EXAMPLE-PLACEHOLDER' \
  --region us-east-1
```

The secret must already exist before `cdk deploy`. The stack reads it via `Secret.fromSecretNameV2` and grants the ECS execution role `secretsmanager:GetSecretValue` only.

## Build, tag, push image

```bash
docker build -t a-rust-vm:PLACEHOLDER_TAG .
# After first deploy, get ECR URI from stack outputs:
aws ecr describe-repositories --repository-names a-rust-vm --region us-east-1 --query 'repositories[0].repositoryUri' --output text
# Example URI: 123456789012.dkr.ecr.us-east-1.amazonaws.com/a-rust-vm
aws ecr get-login-password --region us-east-1 | docker login --username AWS --password-stdin 123456789012.dkr.ecr.us-east-1.amazonaws.com
docker tag a-rust-vm:PLACEHOLDER_TAG 123456789012.dkr.ecr.us-east-1.amazonaws.com/a-rust-vm:PLACEHOLDER_TAG
docker push 123456789012.dkr.ecr.us-east-1.amazonaws.com/a-rust-vm:PLACEHOLDER_TAG
```

Use a new immutable image tag for each build and pass the same tag to CDK.

## Synth / diff / deploy

All synths and deploys require `certificateArn`, `secretName`, `account`, `region`, and `imageTag`.

```bash
cd infra
npm ci
npx cdk synth -c certificateArn=arn:aws:acm:us-east-1:123456789012:certificate/EXAMPLE-PLACEHOLDER \
  -c secretName=prod/openrouter/api-key \
  -c account=123456789012 -c region=us-east-1 \
  -c domainName=asiliano.online \
  -c imageTag=PLACEHOLDER_TAG
npx cdk diff  -c certificateArn=arn:aws:acm:us-east-1:123456789012:certificate/EXAMPLE-PLACEHOLDER \
  -c secretName=prod/openrouter/api-key -c account=123456789012 -c region=us-east-1 -c imageTag=PLACEHOLDER_TAG
npx cdk deploy -c certificateArn=arn:aws:acm:us-east-1:123456789012:certificate/EXAMPLE-PLACEHOLDER \
  -c secretName=prod/openrouter/api-key \
  -c account=123456789012 -c region=us-east-1 \
  -c imageTag=PLACEHOLDER_TAG
```

Environment/region/account and image tag are taken from CDK context or their documented environment variables. Failing to supply required inputs fails synthesis with a clear error.

Environment injected into the task: `OPENROUTER_API_KEY` (from the plaintext Secrets Manager value), `OPENROUTER_MODEL`, `A_RVM_ALLOWED_ORIGIN=https://asiliano.online`, `A_RVM_SECURE_COOKIES=true`, `A_RVM_BIND_ADDRESS=0.0.0.0`, `A_RVM_WEB_PORT=8080`.

## Route 53 / ALB DNS for asiliano.online

After deploy, note the `AlbDnsName` output (e.g. `ARvmStack-Alb-EXAMPLE.elb.us-east-1.amazonaws.com`):

```bash
aws route53 change-resource-record-sets --hosted-zone-id ZONEID_PLACEHOLDER --change-batch '{
  "Changes": [{
    "Action": "UPSERT",
    "ResourceRecordSet": {
      "Name": "asiliano.online",
      "Type": "A",
      "AliasTarget": {
        "DNSName": "dualstack.ARvmStack-Alb-EXAMPLE.elb.us-east-1.amazonaws.com",
        "HostedZoneId": "Z2P70J7EXAMPLE",
        "EvaluateTargetHealth": true
      }
    }
  }]
}'
```

Use the ALB hosted zone ID for your region. Verify `https://asiliano.online/api/v1/system` returns 200.

## Operational limits

- Single Fargate task (`desiredCount=1`, min/max 1). Sessions and VM state are process-local and in-memory; scaling horizontally would split user sessions. Horizontal scaling waits for durable shared session state.
- ALB health check is the session-free `GET /healthz` over HTTP on 8080.
- WAF: AWS Managed Common Rule Set + IP rate limit of 500 requests per 5 minutes per IP (conservative demo limit, documented in `infra/lib/stack.ts`).
- Log retention 30 days.

## Secret rotation / redeploy

```bash
aws secretsmanager put-secret-value --secret-id prod/openrouter/api-key \
  --secret-string 'sk-or-v1-NEW-PLACEHOLDER' --region us-east-1
# Force new deployment to pull latest secret:
aws ecs update-service --cluster $(aws ecs list-clusters --query 'clusterArns[0]' --output text) \
  --service arvm --force-new-deployment --region us-east-1
# Or redeploy image:
docker build -t a-rust-vm:NEW_TAG . && docker push ... && npx cdk deploy -c imageTag=NEW_TAG ...
```

Never commit the secret value. Rotate by updating Secrets Manager; ECS execution role fetches it at task start.
