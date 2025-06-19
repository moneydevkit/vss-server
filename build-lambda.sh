#!/bin/bash

# Build script for VSS Server Lambda deployment
set -e

echo "Building VSS Server for AWS Lambda deployment..."

# Check if we're in the right directory
if [ ! -f "template.yaml" ]; then
    echo "Error: template.yaml not found. Please run this script from the project root."
    exit 1
fi

# Navigate to Rust directory
cd rust

# Install cargo-lambda if not already installed
if ! command -v cargo-lambda &> /dev/null; then
    echo "Installing cargo-lambda..."
    cargo install cargo-lambda
fi

# Build for Lambda with optimizations
echo "Building Rust code for Lambda..."
cargo lambda build --release --features lambda

# Create target directory structure expected by SAM
echo "Preparing Lambda deployment package..."
mkdir -p target/lambda/server
cp target/lambda/server/bootstrap target/lambda/server/ || cp target/lambda/server/server target/lambda/server/bootstrap

# Go back to project root
cd ..

echo "Build complete! Lambda deployment package ready at rust/target/lambda/server/"
echo "You can now deploy with: sam deploy --guided" 