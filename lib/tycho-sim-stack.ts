import * as path from "path";
import * as cdk from "aws-cdk-lib";
import { Construct } from "constructs";
import * as ec2 from "aws-cdk-lib/aws-ec2";
import * as ecs from "aws-cdk-lib/aws-ecs";
import * as logs from "aws-cdk-lib/aws-logs";
import * as ecsPatterns from "aws-cdk-lib/aws-ecs-patterns";
import * as ssm from "aws-cdk-lib/aws-ssm";
import * as elbv2 from "aws-cdk-lib/aws-elasticloadbalancingv2";
import * as route53 from "aws-cdk-lib/aws-route53";
import * as targets from "aws-cdk-lib/aws-route53-targets";
import * as ecr_assets from "aws-cdk-lib/aws-ecr-assets";

export interface AppServiceStackProps extends cdk.StackProps {
  serviceName: string;
  tycho_tvl: string;
  tycho_url: string;
  containerPort?: number;
  environment: string;
  cpu?: number;
  memoryMiB?: number;
  desiredCount?: number;
  healthCheckPath?: string;
  publicLoadBalancer?: boolean;
  assignPublicIp?: boolean;
  certificateArn?: string;
}

export class AppServiceStack extends cdk.Stack {
  constructor(scope: Construct, id: string, props: AppServiceStackProps) {
    super(scope, id, props);

    const vpc = ec2.Vpc.fromVpcAttributes(this, "Vpc", {
      vpcId: cdk.Fn.importValue("VpcId"),
      availabilityZones: cdk.Fn.getAzs(),
      privateSubnetIds: cdk.Fn.split(
        ",",
        cdk.Fn.importValue("PrivateSubnetIds"),
      ),
      publicSubnetIds: cdk.Fn.split(",", cdk.Fn.importValue("PublicSubnetIds")),
    });

    const cluster = new ecs.Cluster(this, "Cluster", {
      vpc,
      clusterName: `${props.serviceName}-cluster`,
    });

    const nlbSg = new ec2.SecurityGroup(this, "NlbSg", {
      vpc,
      allowAllOutbound: true,
    });
    nlbSg.addIngressRule(
      ec2.Peer.ipv4("10.0.0.0/8"),
      ec2.Port.tcp(80),
      "NLB listener",
    );

    const serviceSg = new ec2.SecurityGroup(this, "ServiceSg", {
      vpc,
      allowAllOutbound: true,
    });

    serviceSg.addIngressRule(
      ec2.Peer.securityGroupId(nlbSg.securityGroupId),
      ec2.Port.tcp(3000),
      "Allow NLB to Service",
    );

    const nlb = new elbv2.NetworkLoadBalancer(this, "Nlb", {
      vpc,
      internetFacing: false,
      crossZoneEnabled: true,
      securityGroups: [nlbSg],
    });

    const image = ecs.ContainerImage.fromAsset(path.join(__dirname, ".."), {
      file: "Dockerfile",
      platform: ecr_assets.Platform.LINUX_ARM64,
    });

    const tychoAuthParam =
      ssm.StringParameter.fromSecureStringParameterAttributes(
        this,
        "TychohAuthParam",
        {
          parameterName: "/manual/tycho/api",
          version: 1,
        },
      );
    const logGroup = new logs.LogGroup(this, "LogGroup", {
      logGroupName: `/ecs/${props.serviceName}`,
      retention: logs.RetentionDays.ONE_MONTH,
      removalPolicy: cdk.RemovalPolicy.DESTROY,
    });

    const fargate = new ecsPatterns.NetworkLoadBalancedFargateService(
      this,
      "Service",
      {
        serviceName: props.serviceName,
        healthCheckGracePeriod: cdk.Duration.minutes(5),
        loadBalancer: nlb,
        securityGroups: [serviceSg],
        cluster,
        assignPublicIp: props.assignPublicIp ?? false,
        cpu: props.cpu ?? 512,
        memoryLimitMiB: props.memoryMiB ?? 1024,
        desiredCount: props.desiredCount ?? 1,
        taskImageOptions: {
          image,
          containerPort: props.containerPort ?? 3000,
          environment: {
            TVL_THRESHOLD: props.tycho_tvl,
            TYCHO_URL: props.tycho_url,
            QUOTE_TIMEOUT_MS: "2000",
            POOL_TIMEOUT_NATIVE_MS: "150",
            POOL_TIMEOUT_VM_MS: "500",
            REQUEST_TIMEOUT_MS: "4000",
            GLOBAL_NATIVE_SIM_CONCURRENCY: "8",
            GLOBAL_VM_SIM_CONCURRENCY: "4",
            TOKIO_WORKER_THREADS: "2",
            TOKIO_BLOCKING_THREADS: "12",
            ENABLE_VM_POOLS: "true",
            HOST: "0.0.0.0",
            RPC_URL: "https://eth-mainnet.g.alchemy.com/v2/xBPCeSqiMPLZMbtinEx_K",
            RUST_LOG: "info",
          },
          logDriver: ecs.LogDriver.awsLogs({
            streamPrefix: props.serviceName,
            logGroup,
          }),
          secrets: {
            TYCHO_API_KEY: ecs.Secret.fromSsmParameter(tychoAuthParam),
          },
        },
        runtimePlatform: {
          cpuArchitecture: ecs.CpuArchitecture.ARM64,
          operatingSystemFamily: ecs.OperatingSystemFamily.LINUX,
        },
      },
    );

    fargate.targetGroup.configureHealthCheck({
      protocol: elbv2.Protocol.TCP,
      port: "3000",
      interval: cdk.Duration.seconds(20),
      healthyThresholdCount: 2,
      unhealthyThresholdCount: 2,
    });

    const scaling = fargate.service.autoScaleTaskCount({
      minCapacity: 7,
      maxCapacity: 12,
    });
    scaling.scaleOnCpuUtilization("CpuScaling", {
      targetUtilizationPercent: 60,
    });

    const zone = route53.HostedZone.fromHostedZoneAttributes(
      this,
      "InternalZoneImport",
      {
        hostedZoneId: cdk.Fn.importValue("InternalHostedZoneId"),
        zoneName: cdk.Fn.importValue("InternalHostedZoneName"),
      },
    );

    new route53.ARecord(this, "ServiceDns", {
      zone,
      recordName: "tycho",
      target: route53.RecordTarget.fromAlias(
        new targets.LoadBalancerTarget(fargate.loadBalancer),
      ),
      ttl: cdk.Duration.minutes(10),
    });
    new cdk.CfnOutput(this, "ServiceUrlOut", {
      value: `http${props.certificateArn ? "s" : ""}://${fargate.loadBalancer.loadBalancerDnsName}`,
      exportName: `${props.serviceName}-ServiceURL`,
    });
  }
}
