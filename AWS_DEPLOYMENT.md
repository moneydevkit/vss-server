# VSS Server - AWS Serverless Deployment Guide

This guide explains how to deploy the VSS Server using AWS serverless architecture with DynamoDB, Lambda, and API Gateway.

## Architecture Overview

The serverless VSS server consists of:

- **Amazon DynamoDB**: NoSQL database for storing VSS data with optimistic concurrency control
- **AWS Lambda**: Serverless compute running the VSS application logic
- **Amazon API Gateway**: RESTful API endpoint for client access
- **AWS SAM**: Infrastructure as Code for deployment and management

## Prerequisites

### Required Tools
1. **AWS CLI** - Install and configure with appropriate credentials
   ```bash
   aws configure
   ```

2. **AWS SAM CLI** - For serverless application deployment
   ```bash
   # Install SAM CLI (varies by OS)
   pip install aws-sam-cli
   ```

3. **Rust and Cargo** - Latest stable version
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

4. **cargo-lambda** - For building Rust Lambda functions
   ```bash
   cargo install cargo-lambda
   ```

### AWS Permissions
Your AWS credentials need permissions for:
- DynamoDB (CreateTable, GetItem, PutItem, UpdateItem, DeleteItem, Query)
- Lambda (CreateFunction, UpdateFunction, InvokeFunction)
- API Gateway (CreateRestApi, CreateResource, CreateMethod)
- IAM (CreateRole, AttachRolePolicy)
- CloudFormation (CreateStack, UpdateStack, DeleteStack)

## Build and Deployment

### Step 1: Build for Lambda

Use the provided build script to compile the Rust code for Lambda:

```bash
./build-lambda.sh
```

This script:
- Installs `cargo-lambda` if needed
- Builds the Rust code with Lambda-specific optimizations
- Prepares the deployment package in the correct directory structure

### Step 2: Deploy with SAM

Deploy the infrastructure and application:

```bash
# First deployment (guided)
sam deploy --guided

# Subsequent deployments
sam deploy
```

During guided deployment, you'll be prompted for:
- **Stack Name**: e.g., `vss-server-dev`
- **AWS Region**: e.g., `us-east-1`
- **Environment**: `dev`, `staging`, or `prod`
- **Confirm changes**: Review the resources being created

### Step 3: Get API Endpoint

After deployment, SAM will output the API Gateway URL:

```
Outputs:
VssApiUrl: https://abc123.execute-api.us-east-1.amazonaws.com/dev/
```

## API Usage

The deployed API follows RESTful conventions:

### Get Object
```bash
GET /v1/{store_id}/{key}
Authorization: Bearer <token>
```

### Put Object
```bash
POST /v1/{store_id}/{key}
Authorization: Bearer <token>
Content-Type: application/json

{
  "transaction_items": [
    {
      "key": "example-key",
      "value": "base64-encoded-data",
      "version": 1
    }
  ]
}
```

### Delete Object
```bash
DELETE /v1/{store_id}/{key}
Authorization: Bearer <token>
Content-Type: application/json

{
  "key_value": {
    "key": "example-key",
    "version": 2
  }
}
```

### List Key Versions
```bash
GET /v1/{store_id}/list?key_prefix=optional&page_size=100
Authorization: Bearer <token>
```

## Configuration

### Environment Variables

The Lambda function uses these environment variables (set automatically by SAM):

- `VSS_DYNAMODB_TABLE_NAME`: DynamoDB table name
- `AWS_REGION`: AWS region
- `RUST_LOG`: Logging level (info, debug, warn, error)

### Local Development

For local development, you can still run the traditional HTTP server:

```bash
cd rust
cargo run --bin server ../rust/server/vss-server-config.toml
```

The configuration file supports both DynamoDB and PostgreSQL backends:

```toml
[server_config]
host = "127.0.0.1"
port = 8080

[dynamodb_config]
table_name = "vss-table-dev"
region = "us-east-1"

# Optional: PostgreSQL fallback
[postgresql_config]
host = "localhost"
port = 5432
database = "postgres"
username = "postgres"
password = "postgres"
```

## Testing

### Integration Tests

Test the deployed API with curl:

```bash
# Set your API endpoint
API_URL="https://your-api-id.execute-api.us-east-1.amazonaws.com/dev"

# Test GET (should return 404 for non-existent key)
curl -X GET "$API_URL/v1/store123/test-key" \
  -H "Authorization: Bearer your-token"

# Test PUT (create new item)
curl -X POST "$API_URL/v1/store123/test-key" \
  -H "Authorization: Bearer your-token" \
  -H "Content-Type: application/json" \
  -d '{
    "transaction_items": [{
      "key": "test-key",
      "value": "dGVzdCBkYXRh", 
      "version": 0
    }]
  }'

# Test GET (should return the item)
curl -X GET "$API_URL/v1/store123/test-key" \
  -H "Authorization: Bearer your-token"
```

### Load Testing

For production readiness, test with tools like:
- Apache Bench (ab)
- Artillery.io
- AWS Load Testing Solution

## Monitoring and Observability

### CloudWatch Metrics

Monitor these key metrics:
- **Lambda**: Duration, Errors, Invocations, Throttles
- **API Gateway**: Count, Latency, 4xxError, 5xxError
- **DynamoDB**: ConsumedReadCapacityUnits, ConsumedWriteCapacityUnits

### CloudWatch Logs

Lambda logs are automatically sent to CloudWatch Logs:
```bash
aws logs tail "/aws/lambda/vss-function-dev" --follow
```

### X-Ray Tracing

Enable X-Ray tracing in the SAM template for distributed tracing:
```yaml
VssFunction:
  Type: AWS::Serverless::Function
  Properties:
    Tracing: Active
```

## Cost Optimization

### DynamoDB
- Uses PAY_PER_REQUEST billing mode
- Consider switching to PROVISIONED if usage patterns are predictable
- Enable DynamoDB auto-scaling for provisioned mode

### Lambda
- Memory: 512MB (adjust based on performance testing)
- Timeout: 30 seconds (sufficient for VSS operations)

### API Gateway
- Regional endpoints (cheaper than Edge)
- Consider REST API vs HTTP API based on feature needs

## Security Best Practices

### API Gateway
- Enable API Keys for production
- Implement rate limiting (configured in template)
- Use AWS WAF for DDoS protection
- Enable CORS only for trusted origins

### Lambda
- Principle of least privilege for IAM roles
- Enable VPC for additional network isolation (if needed)
- Use environment variables for secrets (or AWS Secrets Manager)

### DynamoDB
- Encryption at rest enabled by default
- Consider encryption in transit for sensitive data
- Use VPC endpoints for private access

## Troubleshooting

### Common Issues

1. **Build Fails**: Ensure `cargo-lambda` is installed and Rust is up to date
2. **Deployment Fails**: Check AWS credentials and permissions
3. **Lambda Timeout**: Increase timeout in SAM template
4. **DynamoDB Access Denied**: Verify IAM role permissions

### Debug Steps

1. Check CloudWatch Logs for Lambda errors
2. Test DynamoDB access with AWS CLI
3. Verify API Gateway configuration
4. Use SAM local for local testing

```bash
# Test locally with SAM
sam local start-api
```

## Cleanup

To remove all AWS resources:

```bash
sam delete --stack-name vss-server-dev
```

This will delete:
- DynamoDB table (⚠️ data will be lost)
- Lambda function
- API Gateway
- IAM roles
- CloudWatch logs (after retention period)

## Production Considerations

### High Availability
- Multi-AZ DynamoDB (automatic)
- Lambda runs in multiple AZs automatically
- API Gateway is highly available by default

### Backup and Recovery
- DynamoDB Point-in-Time Recovery enabled
- Consider DynamoDB Global Tables for multi-region
- CloudFormation stack can be recreated in disaster recovery

### Compliance
- Enable CloudTrail for audit logging
- Use AWS Config for compliance monitoring
- Consider AWS Organizations for account management 