import * as cdk from "aws-cdk-lib";
import { PipelineStack } from "../lib/pipeline/pipeline-stack";

const app = new cdk.App();

new PipelineStack(app, "tycho-simulator-pipeline", {
  env: {
    account: process.env.CDK_DEFAULT_ACCOUNT,
    region: process.env.CDK_DEFAULT_REGION,
  },
});

app.synth();
