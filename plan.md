Preamble for the Agent

Project Objective: Adapt the open-source vss-server (https://github.com/lightningdevkit/vss-server) to use a serverless AWS backend instead of a local file-based database. The new architecture will use Amazon DynamoDB for storage, AWS Lambda for compute, and Amazon API Gateway for handling requests.

Key Requirements:

    The core versioning logic (optimistic concurrency control) must be preserved.
    The solution must be scalable, secure, and follow AWS best practices.
    All infrastructure should be defined as code for repeatability.

Prerequisites

    An AWS account with administrative or sufficient IAM permissions to create the necessary resources.
    AWS CLI installed and configured.
    An Infrastructure as Code (IaC) tool installed, such as AWS SAM CLI, AWS CDK, or Terraform. AWS SAM is recommended for its simplicity in defining serverless applications.
    The necessary toolchain for the application language (e.g., Rustup and Cargo if using Rust).
    GitHub repository with Actions enabled for automated deployment.
    Grafana instance configured for monitoring (cloud or self-hosted).

Phase 1: Infrastructure Setup (Infrastructure as Code)

Define all the following resources using your chosen IaC tool (e.g., in a template.yaml for AWS SAM).

    Task 1.1: Define the DynamoDB Table
        Logical ID: VssTable
        Table Name: vss-table
        Primary Key:
            Partition Key (PK): namespace (Type: String)
            Sort Key (SK): key (Type: String)
        Billing Mode: PAY_PER_REQUEST (Provisioned Throughput can be configured later if traffic patterns are predictable).
        Encryption: Enable Server-Side Encryption (SSE) using an AWS-owned key (AWS_KMS). This is a best practice.

    Task 1.2: Define the IAM Role for the Lambda Function
        Logical ID: VssLambdaRole
        Policy Statement 1 (DynamoDB Access):
            Effect: Allow
            Actions:
                dynamodb:GetItem
                dynamodb:PutItem
                dynamodb:UpdateItem
                dynamodb:DeleteItem
            Resource: The ARN of the VssTable created in Task 1.1.
        Policy Statement 2 (Logging Access):
            Effect: Allow
            Actions:
                logs:CreateLogGroup
                logs:CreateLogStream
                logs:PutLogEvents
            Resource: arn:aws:logs:*:*:* (as per standard Lambda practice).

Phase 2: Application Logic Adaptation

Modify the vss-server source code to interact with DynamoDB instead of the local sled database.

    Task 2.1: Add AWS SDK Dependency
        In your project's dependency file (e.g., Cargo.toml for Rust), add the required AWS SDK for DynamoDB v3.

    Task 2.2: Implement the DynamoDB KV Store
        Create a new module to replace the existing kv::Store implementation. This new module will contain a struct that holds the DynamoDB client.
        Get Operation: Implement the get(namespace, key) method. This method should perform a GetItem API call to DynamoDB using the provided namespace (as PK) and key (as SK).
        Put Operation (The Core Task): Implement the put(namespace, key, data, expected_version) method.
            This method must use a PutItem or UpdateItem API call.
            It must construct a ConditionExpression to enforce optimistic locking.
                If creating a new item (expected_version is 0 or null), the condition should be attribute_not_exists(version).
                If updating an existing item, the condition must be version = :expected_version.
            On a successful write, the version attribute in DynamoDB must be atomically incremented. The last_modified timestamp should also be set to the current time.
            The implementation must gracefully handle the ConditionalCheckFailedException returned by DynamoDB when the version check fails, returning an appropriate error to the caller.

    Task 2.3: Integrate the New Storage Backend
        Modify the main application logic to instantiate and use your new DynamoDB storage backend instead of the sled backend.

Phase 3: Lambda and API Gateway Integration

Define the Lambda function and API Gateway within your IaC template.

    Task 3.1: Define the Lambda Function Resource
        Logical ID: VssFunction
        Handler: Specify the function handler (e.g., bootstrap for Rust's custom runtime).
        Runtime: Specify the appropriate runtime (e.g., provided.al2 for custom Rust runtimes).
        Code URI: Point to the directory containing the compiled application code.
        Role: Attach the ARN of the VssLambdaRole created in Task 1.2.
        Environment Variables: Pass the VssTable name as an environment variable to the function so the code isn't hardcoded.

    Task 3.2: Define the API Gateway
        Use the AWS::Serverless::Api resource type in SAM (or equivalent in other IaC tools).
        Define the API events that trigger the VssFunction. A good RESTful pattern would be:
            Get State: GET /v1/{namespace}/{key}
            Update State: POST /v1/{namespace}/{key}
        Use Lambda Proxy Integration for simplicity and performance.

    Task 3.3: Deploy the Stack
        Configure GitHub Actions for automated deployment.
        Create a deployment workflow that builds the application binary and deploys using your IaC tool.
        Set up environment-specific deployments (staging/production) with appropriate AWS credentials and environment variables.

    Task 3.4: GitHub Actions Deployment Setup
        Create `.github/workflows/deploy.yml` with separate jobs for staging and production.
        Configure AWS credentials using GitHub Secrets (AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY).
        Set up environment-specific variables for table names, stack names, and regions.
        Implement deployment triggers (e.g., staging on push to main, production on release tags).

Phase 4: Testing and Validation

    Task 4.1: Create an Integration Test Suite
        Using a tool like curl, Postman, or an automated testing framework, create a suite of tests that call the deployed API Gateway endpoint.

    Task 4.2: Execute Test Scenarios
        Scenario 1 (Create): POST a new key with an initial payload. Verify a 200 OK response and that the item exists in the DynamoDB table with version = 1.
        Scenario 2 (Read): GET the key created in Scenario 1. Verify the payload and version are correct.
        Scenario 3 (Successful Update): POST to the same key with a new payload, providing the correct version (e.g., 1). Verify a 200 OK response and that the item in DynamoDB is updated with the new data and version = 2.
        Scenario 4 (Failed Update): POST to the same key again, but provide the old version (e.g., 1). Verify a 409 Conflict (or similar) error response, indicating the conditional check failed.
        Scenario 5 (Not Found): GET a key that does not exist. Verify a 404 Not Found response.

Phase 5: Production Hardening & Final Touches

    Task 5.1: Configure API Gateway Security
        Enable and configure throttling (Rate Limit and Burst Limit) to protect your backend.
        Create a Usage Plan and require API Keys for all requests.
        (Optional but recommended) Attach AWS WAF to the API Gateway for protection against common web exploits.

    Task 5.2: Implement Monitoring and Alarms
        Configure Grafana integration to monitor key metrics cost-effectively:
            Set up CloudWatch data source in Grafana to pull AWS metrics
            Create dashboards for API Gateway: Count, Latency, 4xxError, 5xxError
            Create dashboards for Lambda: Invocations, Errors, Duration, Throttles
        Configure Grafana alerts for high Lambda Errors or high API Gateway 5xxError rates.
        Note: Use standard CloudWatch metrics where possible to minimize monitoring costs.

    Task 5.3: Update Documentation
        Update the project's README.md to describe the new AWS-based architecture.
        Provide clear instructions on how to configure, build, and deploy the service using the IaC template.
        Document the API endpoints, including expected request/response formats and status codes.