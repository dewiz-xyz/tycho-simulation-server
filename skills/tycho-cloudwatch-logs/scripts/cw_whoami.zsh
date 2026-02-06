#!/usr/bin/env zsh
set -euo pipefail

script_dir="$(cd "$(dirname "$0")" && pwd)"
source "$script_dir/cw_env.zsh"

echo "AWS_REGION=${AWS_REGION:-${AWS_DEFAULT_REGION:-}}"
echo "TYCHO_LOG_GROUP=$TYCHO_LOG_GROUP"

# Uses the same env-loading logic as the other cw_* scripts (incl. .env fallback).
aws sts get-caller-identity --output json --no-cli-pager

