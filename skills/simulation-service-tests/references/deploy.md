# Deploy notes (CDK + Docker)

This repo ships the Rust service and an AWS CDK (TypeScript) deployment for Fargate.

## Docker
- `Dockerfile` builds the release binary (`tycho-price-service`) into a container image.
- Common local sanity check:
  - `cargo build --release`
  - `docker build -t tycho-price-service .`

## CDK (TypeScript)
- CDK entrypoint: `bin/index.ts` (wired via `cdk.json`).
- Install deps:
  - `npm ci`
- Synthesize CloudFormation:
  - `npx cdk synth`
- Review changes:
  - `npx cdk diff`
- Deploy (requires AWS credentials configured locally):
  - `npx cdk deploy`

## Secrets/config
- Never hardcode `TYCHO_API_KEY`.
- CDK pulls `TYCHO_API_KEY` from SSM (repo convention: `/manual/tycho/api`).

