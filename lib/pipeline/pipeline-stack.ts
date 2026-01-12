import * as cdk from "aws-cdk-lib";
import * as pipelines from "aws-cdk-lib/pipelines";
import { Construct } from "constructs";
import { AppServiceStack, AppServiceStackProps } from "../tycho-sim-stack";
import * as ssm from "aws-cdk-lib/aws-ssm";
import * as iam from "aws-cdk-lib/aws-iam";
import * as codebuild from "aws-cdk-lib/aws-codebuild";

class AppServiceStage extends cdk.Stage {
  constructor(scope: Construct, id: string, props: AppServiceStackProps) {
    super(scope, id, props);

    new AppServiceStack(this, "tycho-sim", props);
  }
}

export class PipelineStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props?: cdk.StackProps) {
    super(scope, id, props);

    const githubConnectionArn = ssm.StringParameter.valueFromLookup(
      this,
      "/cicd/github_codestar",
    );
    const testnet_id = ssm.StringParameter.valueFromLookup(
      this,
      "/cicd/testnet_account_id",
    );

    const mainnet_id = ssm.StringParameter.valueFromLookup(
      this,
      "/cicd/mainnet_account_id",
    );

    const synth = new pipelines.CodeBuildStep("Synth", {
      input: pipelines.CodePipelineSource.connection(
        "dewiz-xyz/tycho-simulation-server",
        "master",
        {
          connectionArn: githubConnectionArn,
        },
      ),
      installCommands: ["npm i -g aws-cdk"],
      commands: ["npm ci", "npx cdk synth"],
      rolePolicyStatements: [
        new iam.PolicyStatement({
          actions: ["ssm:GetParameter"],
          resources: [
            `arn:aws:ssm:${this.region}:${this.account}:parameter/cicd/*`,
          ],
        }),
      ],
    });

    const pipeline = new pipelines.CodePipeline(this, "Pipeline", {
      pipelineName: "tycho-sim-pipeline",
      crossAccountKeys: true,
      synth,
      assetPublishingCodeBuildDefaults: {
        buildEnvironment: {
          buildImage: codebuild.LinuxBuildImage.AMAZON_LINUX_2_ARM_3,
        },
      },
    });

    // Staging / Testnet properties
    pipeline.addStage(
      new AppServiceStage(this, "tycho-simulator-testnet", {
        env: { account: testnet_id, region: "eu-central-1" },
        serviceName: "tycho-simulator",
        environment: "testnet",
        publicLoadBalancer: false,
        cpu: 4096,
        memoryMiB: 8192,
        desiredCount: 10,
        tycho_tvl: "10",
        tycho_url: "tycho-beta.propellerheads.xyz",
      }),
    );

    // Production / Mainnet properties
    pipeline.addStage(
      new AppServiceStage(this, "tycho-simulator-mainnet", {
        env: { account: mainnet_id, region: "eu-central-1" },
        serviceName: "tycho-simulator",
        environment: "mainnet",
        publicLoadBalancer: false,
        cpu: 512,
        memoryMiB: 1024,
        tycho_tvl: "50",
        tycho_url: "tycho-beta.propellerheads.xyz",
      }),
    );
  }
}
