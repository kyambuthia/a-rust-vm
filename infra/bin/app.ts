import * as cdk from "aws-cdk-lib";
import { ARvmStack } from "../lib/stack";

function requireContext(app: cdk.App, key: string): string {
  const value = app.node.tryGetContext(key) as string | undefined;
  if (value && String(value).trim().length > 0) {
    return String(value).trim();
  }
  const envValue = process.env[key.toUpperCase()] ?? process.env[`CDK_CONTEXT_${key}`];
  if (envValue && envValue.trim().length > 0) {
    return envValue.trim();
  }
  return "";
}

const app = new cdk.App();

const certificateArn = requireContext(app, "certificateArn");
const secretName = requireContext(app, "secretName");
const account = requireContext(app, "account") || process.env.CDK_DEFAULT_ACCOUNT || "";
const region = requireContext(app, "region") || process.env.CDK_DEFAULT_REGION || "";
const imageTag = requireContext(app, "imageTag");

if (!certificateArn) {
  throw new Error(
    "Missing required context 'certificateArn'. Provide via -c certificateArn=arn:aws:acm:REGION:ACCOUNT:certificate/EXAMPLE or env CERTIFICATEARN. Do not create placeholder certificates."
  );
}
if (!secretName) {
  throw new Error(
    "Missing required context 'secretName'. Provide via -c secretName=my-secret-name or env SECRETNAME. The Secrets Manager secret must already exist."
  );
}
if (!account || !region) {
  throw new Error(
    "Missing required AWS account/region. Provide -c account=ACCOUNT_ID -c region=REGION or set CDK_DEFAULT_ACCOUNT/CDK_DEFAULT_REGION."
  );
}
if (!imageTag) {
  throw new Error(
    "Missing required context 'imageTag'. Use a unique immutable ECR tag, for example -c imageTag=2026-09-02-01."
  );
}

const domainName = (app.node.tryGetContext("domainName") as string | undefined) ?? process.env.DOMAINNAME ?? "asiliano.online";
const openRouterModel = (app.node.tryGetContext("openRouterModel") as string | undefined) ?? process.env.OPENROUTERMODEL ?? "~openai/gpt-latest";

new ARvmStack(app, "ARvmStack", {
  env: {
    account,
    region,
  },
  certificateArn,
  secretName,
  region,
  domainName: String(domainName),
  imageTag: String(imageTag),
  openRouterModel: String(openRouterModel),
});
