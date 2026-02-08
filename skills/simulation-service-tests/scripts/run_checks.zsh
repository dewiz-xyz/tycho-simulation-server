#!/bin/zsh
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: run_checks.zsh --repo <path> [--fmt-check] [--cdk] [--docker]

Run a CI-like verification pass for this repo.

Rust (always):
  - cargo fmt (or --check)
  - cargo clippy --all-targets --all-features -- -D warnings
  - cargo test
  - cargo build --release

Optional:
  --cdk    Run npm ci (if needed) + npx cdk synth
  --docker Build Docker image (docker build -t tycho-price-service .)

Options:
  --repo       Repo root containing Cargo.toml
  --fmt-check  Use cargo fmt --check instead of formatting in place
  -h, --help   Show this help
USAGE
}

repo=""
fmt_check="false"
run_cdk="false"
run_docker="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo)
      repo="$2"
      shift 2
      ;;
    --fmt-check)
      fmt_check="true"
      shift 1
      ;;
    --cdk)
      run_cdk="true"
      shift 1
      ;;
    --docker)
      run_docker="true"
      shift 1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "$repo" ]]; then
  echo "Error: --repo is required" >&2
  usage
  exit 2
fi

repo="$(cd "$repo" && pwd)"
cd "$repo"

echo "Repo: $repo"

if [[ "$fmt_check" == "true" ]]; then
  cargo fmt --check
else
  cargo fmt
fi

cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release

if [[ "$run_docker" == "true" ]]; then
  if ! command -v docker >/dev/null 2>&1; then
    echo "Error: docker not found in PATH" >&2
    exit 1
  fi
  docker build -t tycho-price-service .
fi

if [[ "$run_cdk" == "true" ]]; then
  if ! command -v npm >/dev/null 2>&1; then
    echo "Error: npm not found in PATH" >&2
    exit 1
  fi
  if ! command -v npx >/dev/null 2>&1; then
    echo "Error: npx not found in PATH" >&2
    exit 1
  fi

  if [[ ! -d node_modules ]]; then
    npm ci
  fi
  npx cdk synth
fi

echo "Checks complete."

