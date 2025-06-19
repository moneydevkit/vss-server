//! Hosts VSS http-server implementation.
//!
//! VSS is an open-source project designed to offer a server-side cloud storage solution specifically
//! tailored for noncustodial Lightning supporting mobile wallets. Its primary objective is to
//! simplify the development process for Lightning wallets by providing a secure means to store
//! and manage the essential state required for Lightning Network (LN) operations.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(missing_docs)]

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::signal::unix::SignalKind;

use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
#[cfg(feature = "lambda")]
use tower::ServiceBuilder;

use crate::vss_service::VssService;
use api::auth::{Authorizer, NoopAuthorizer};
use api::kv_store::KvStore;
use impls::dynamodb_store::DynamoDbBackendImpl;
use impls::postgres_store::PostgresBackendImpl;

pub(crate) mod util;
pub(crate) mod vss_service;

/// Create DynamoDB backend store
async fn create_dynamodb_store(table_name: String) -> Result<Arc<dyn KvStore>, Box<dyn std::error::Error>> {
	    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
	let client = aws_sdk_dynamodb::Client::new(&config);
	let store = DynamoDbBackendImpl::new(client, table_name);
	Ok(Arc::new(store))
}

/// Create PostgreSQL backend store
async fn create_postgres_store(connection_string: &str) -> Result<Arc<dyn KvStore>, Box<dyn std::error::Error>> {
	let store = PostgresBackendImpl::new(connection_string).await?;
	Ok(Arc::new(store))
}

/// Run as AWS Lambda function
#[cfg(feature = "lambda")]
async fn run_lambda() -> Result<(), Box<dyn std::error::Error>> {
	use lambda_http::{run, service_fn, Request, RequestExt, Response, Body, Error};
	use api::types::{GetObjectRequest, PutObjectRequest, DeleteObjectRequest};

	// Get table name from environment variable (set by SAM template)
	let table_name = std::env::var("VSS_DYNAMODB_TABLE_NAME")
		.expect("VSS_DYNAMODB_TABLE_NAME environment variable must be set for Lambda deployment");

	let store = create_dynamodb_store(table_name)
		.await
		.expect("Failed to create DynamoDB store");

	let authorizer: Arc<dyn Authorizer> = Arc::new(NoopAuthorizer {});

	// Create a simple lambda handler that routes requests to the appropriate VSS operations
	let handler = service_fn(move |event: Request| {
		let store = Arc::clone(&store);
		let authorizer = Arc::clone(&authorizer);
		
		async move {
			// Parse path and method
			let method = event.method();
			let path = event.uri().path();
			let path_params = event.path_parameters();
			
			// Extract user token from Authorization header
			let user_token = event.headers()
				.get("authorization")
				.and_then(|h| h.to_str().ok())
				.and_then(|s| s.strip_prefix("Bearer "))
				.unwrap_or("default_user")
				.to_string();

			let response = match (method.as_str(), extract_path_components(path)) {
				("GET", Some((store_id, key))) => {
					// GET object
					let request = GetObjectRequest { store_id, key };
					match store.get(user_token, request).await {
						Ok(response) => {
							Response::builder()
								.status(200)
								.header("content-type", "application/json")
								.body(Body::from(serde_json::to_string(&response).unwrap()))
								.unwrap()
						}
						Err(e) => error_response(e)
					}
				}
				("POST", Some((store_id, _))) => {
					// PUT object
					let body_bytes = match event.body() {
						Body::Text(text) => text.as_bytes(),
						Body::Binary(bytes) => bytes,
						Body::Empty => &[],
					};
					
					match serde_json::from_slice::<PutObjectRequest>(body_bytes) {
						Ok(mut request) => {
							request.store_id = store_id;
							match store.put(user_token, request).await {
								Ok(response) => {
									Response::builder()
										.status(201)
										.header("content-type", "application/json")
										.body(Body::from(serde_json::to_string(&response).unwrap()))
										.unwrap()
								}
								Err(e) => error_response(e)
							}
						}
						Err(_) => {
							Response::builder()
								.status(400)
								.body(Body::from("Invalid JSON in request body"))
								.unwrap()
						}
					}
				}
				("DELETE", Some((store_id, _))) => {
					// DELETE object
					let body_bytes = match event.body() {
						Body::Text(text) => text.as_bytes(),
						Body::Binary(bytes) => bytes,
						Body::Empty => &[],
					};
					
					match serde_json::from_slice::<DeleteObjectRequest>(body_bytes) {
						Ok(mut request) => {
							request.store_id = store_id;
							match store.delete(user_token, request).await {
								Ok(response) => {
									Response::builder()
										.status(200)
										.header("content-type", "application/json")
										.body(Body::from(serde_json::to_string(&response).unwrap()))
										.unwrap()
								}
								Err(e) => error_response(e)
							}
						}
						Err(_) => {
							Response::builder()
								.status(400)
								.body(Body::from("Invalid JSON in request body"))
								.unwrap()
						}
					}
				}
				_ => {
					Response::builder()
						.status(404)
						.body(Body::from("Not Found"))
						.unwrap()
				}
			};

			Ok::<Response<Body>, Error>(response)
		}
	});

	run(handler).await?;
	Ok(())
}

/// Extract store_id and key from path like /v1/store123/key456
#[cfg(feature = "lambda")]
fn extract_path_components(path: &str) -> Option<(String, String)> {
	let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
	if parts.len() >= 3 && parts[0] == "v1" {
		Some((parts[1].to_string(), parts[2].to_string()))
	} else {
		None
	}
}

/// Create error response from VssError
#[cfg(feature = "lambda")]
fn error_response(err: api::error::VssError) -> Response<Body> {
	use api::error::VssError;
	use lambda_http::{Response, Body};
	
	let (status, message) = match err {
		VssError::NoSuchKeyError(msg) => (404, msg),
		VssError::ConflictError(msg) => (409, msg),
		VssError::InvalidRequestError(msg) => (400, msg),
		VssError::AuthError(msg) => (401, msg),
	};
	
	Response::builder()
		.status(status)
		.header("content-type", "application/json")
		.body(Body::from(format!("{{\"error\": \"{}\"}}", message)))
		.unwrap()
}

/// Run as traditional HTTP server
async fn run_http_server(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
	let config = util::config::load_config(config_path)?;

	let addr: SocketAddr =
		format!("{}:{}", config.server_config.host, config.server_config.port).parse()?;

	// Create store based on configuration - prefer DynamoDB if configured
	let store: Arc<dyn KvStore> = if let Some(dynamodb_config) = config.dynamodb_config {
		println!("Using DynamoDB backend with table: {}", dynamodb_config.get_table_name());
		create_dynamodb_store(dynamodb_config.get_table_name()).await?
	} else if let Some(postgresql_config) = config.postgresql_config {
		println!("Using PostgreSQL backend");
		create_postgres_store(&postgresql_config.to_connection_string()).await?
	} else {
		return Err("Either dynamodb_config or postgresql_config must be provided in configuration".into());
	};

	let authorizer: Arc<dyn Authorizer> = Arc::new(NoopAuthorizer {});

	let runtime = tokio::runtime::Handle::current();
	let mut sigterm_stream = tokio::signal::unix::signal(SignalKind::terminate())?;
	
	let rest_svc_listener = TcpListener::bind(&addr).await?;
	println!("VSS Server listening on {}", addr);

	loop {
		tokio::select! {
			res = rest_svc_listener.accept() => {
				match res {
					Ok((stream, _)) => {
						let io_stream = TokioIo::new(stream);
						let vss_service = VssService::new(Arc::clone(&store), Arc::clone(&authorizer));
						runtime.spawn(async move {
							if let Err(err) = http1::Builder::new().serve_connection(io_stream, vss_service).await {
								eprintln!("Failed to serve connection: {}", err);
							}
						});
					},
					Err(e) => eprintln!("Failed to accept connection: {}", e),
				}
			}
			_ = tokio::signal::ctrl_c() => {
				println!("Received CTRL-C, shutting down..");
				break;
			}
			_ = sigterm_stream.recv() => {
				println!("Received SIGTERM, shutting down..");
				break;
			}
		}
	}
	Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
	// Check if running in Lambda environment
	if std::env::var("AWS_LAMBDA_FUNCTION_NAME").is_ok() {
		#[cfg(feature = "lambda")]
		{
			println!("Running in Lambda mode");
			return run_lambda().await;
		}
		#[cfg(not(feature = "lambda"))]
		{
			return Err("Lambda runtime not enabled. Compile with --features lambda".into());
		}
	}

	// Traditional HTTP server mode
	let args: Vec<String> = std::env::args().collect();
	if args.len() != 2 {
		eprintln!("Usage: {} <config-file-path>", args[0]);
		std::process::exit(1);
	}

	if let Err(e) = run_http_server(&args[1]).await {
		eprintln!("Server error: {}", e);
		std::process::exit(1);
	}

	Ok(())
}
