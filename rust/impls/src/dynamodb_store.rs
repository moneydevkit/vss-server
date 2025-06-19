use api::error::VssError;
use api::kv_store::{KvStore, GLOBAL_VERSION_KEY, INITIAL_RECORD_VERSION};
use api::types::{
    DeleteObjectRequest, DeleteObjectResponse, GetObjectRequest, GetObjectResponse, KeyValue,
    ListKeyVersionsRequest, ListKeyVersionsResponse, PutObjectRequest, PutObjectResponse,
};
use async_trait::async_trait;
use aws_sdk_dynamodb::{Client as DynamoDbClient};
use aws_sdk_dynamodb::types::{AttributeValue, ReturnValue};
use aws_sdk_dynamodb::operation::put_item::PutItemError;
use aws_sdk_dynamodb::operation::update_item::UpdateItemError;
use aws_sdk_dynamodb::operation::delete_item::DeleteItemError;
use bytes::Bytes;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The maximum number of key versions that can be returned in a single page.
pub const LIST_KEY_VERSIONS_MAX_PAGE_SIZE: i32 = 100;

/// The maximum number of items allowed in a single `PutObjectRequest`.
pub const MAX_PUT_REQUEST_ITEM_COUNT: usize = 1000;

/// DynamoDB record structure for VSS data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VssDbRecord {
    pub(crate) namespace: String,    // user_token + store_id combined
    pub(crate) key: String,
    pub(crate) value: Vec<u8>,
    pub(crate) version: i64,
    pub(crate) created_at: String,
    pub(crate) last_updated_at: String,
}

/// A [DynamoDB](https://aws.amazon.com/dynamodb/) based backend implementation for VSS.
pub struct DynamoDbBackendImpl {
    client: DynamoDbClient,
    table_name: String,
}

impl DynamoDbBackendImpl {
    /// Constructs a [`DynamoDbBackendImpl`] using the provided DynamoDB client and table name.
    pub fn new(client: DynamoDbClient, table_name: String) -> Self {
        DynamoDbBackendImpl { client, table_name }
    }

    /// Creates a namespace key by combining user_token and store_id
    fn create_namespace(&self, user_token: &str, store_id: &str) -> String {
        format!("{}#{}", user_token, store_id)
    }

    /// Builds a VssDbRecord from the input parameters
    fn build_vss_record(&self, user_token: String, store_id: String, kv: KeyValue) -> VssDbRecord {
        let now = Utc::now().to_rfc3339();
        VssDbRecord {
            namespace: self.create_namespace(&user_token, &store_id),
            key: kv.key,
            value: kv.value.to_vec(),
            version: kv.version,
            created_at: now.clone(),
            last_updated_at: now,
        }
    }

    /// Converts a DynamoDB item to a KeyValue
    fn item_to_key_value(&self, item: &HashMap<String, AttributeValue>) -> Result<KeyValue, VssError> {
        let key = item
            .get("key")
            .and_then(|v| v.as_s().ok())
            .ok_or_else(|| VssError::InvalidRequestError("Missing key in DynamoDB item".to_string()))?;

        let value_bytes = item
            .get("value")
            .and_then(|v| v.as_b().ok())
            .map(|b| Bytes::from(b.clone().into_inner()))
            .unwrap_or_else(|| Bytes::new());

        let version = item
            .get("version")
            .and_then(|v| v.as_n().ok())
            .and_then(|n| n.parse::<i64>().ok())
            .unwrap_or(0);

        Ok(KeyValue {
            key: key.clone(),
            value: value_bytes,
            version,
        })
    }

    /// Executes a conditional put operation for new items (version 0)
    async fn execute_conditional_insert(&self, vss_record: &VssDbRecord) -> Result<(), VssError> {
        let mut item = HashMap::new();
        item.insert("namespace".to_string(), AttributeValue::S(vss_record.namespace.clone()));
        item.insert("key".to_string(), AttributeValue::S(vss_record.key.clone()));
        item.insert("value".to_string(), AttributeValue::B(aws_sdk_dynamodb::primitives::Blob::new(vss_record.value.clone())));
        item.insert("version".to_string(), AttributeValue::N(INITIAL_RECORD_VERSION.to_string()));
        item.insert("created_at".to_string(), AttributeValue::S(vss_record.created_at.clone()));
        item.insert("last_updated_at".to_string(), AttributeValue::S(vss_record.last_updated_at.clone()));

        let result = self
            .client
            .put_item()
            .table_name(&self.table_name)
            .set_item(Some(item))
            .condition_expression("attribute_not_exists(#key)")
            .expression_attribute_names("#key", "key")
            .send()
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => {
                if let Some(PutItemError::ConditionalCheckFailedException(_)) = err.as_service_error() {
                    Err(VssError::ConflictError("Item already exists".to_string()))
                } else {
                    Err(VssError::InvalidRequestError(format!("DynamoDB error: {}", err)))
                }
            }
        }
    }

    /// Executes a conditional update operation for existing items
    async fn execute_conditional_update(&self, vss_record: &VssDbRecord) -> Result<(), VssError> {
        let result = self
            .client
            .update_item()
            .table_name(&self.table_name)
            .key("namespace", AttributeValue::S(vss_record.namespace.clone()))
            .key("key", AttributeValue::S(vss_record.key.clone()))
            .update_expression("SET #value = :value, #version = :new_version, #last_updated_at = :last_updated_at")
            .condition_expression("#version = :expected_version")
            .expression_attribute_names("#value", "value")
            .expression_attribute_names("#version", "version")
            .expression_attribute_names("#last_updated_at", "last_updated_at")
            .expression_attribute_values(":value", AttributeValue::B(aws_sdk_dynamodb::primitives::Blob::new(vss_record.value.clone())))
            .expression_attribute_values(":new_version", AttributeValue::N((vss_record.version + 1).to_string()))
            .expression_attribute_values(":expected_version", AttributeValue::N(vss_record.version.to_string()))
            .expression_attribute_values(":last_updated_at", AttributeValue::S(vss_record.last_updated_at.clone()))
            .send()
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => {
                if let Some(UpdateItemError::ConditionalCheckFailedException(_)) = err.as_service_error() {
                    Err(VssError::ConflictError("Version mismatch".to_string()))
                } else {
                    Err(VssError::InvalidRequestError(format!("DynamoDB error: {}", err)))
                }
            }
        }
    }

    /// Executes a non-conditional upsert operation (version -1)
    async fn execute_non_conditional_upsert(&self, vss_record: &VssDbRecord) -> Result<(), VssError> {
        let mut item = HashMap::new();
        item.insert("namespace".to_string(), AttributeValue::S(vss_record.namespace.clone()));
        item.insert("key".to_string(), AttributeValue::S(vss_record.key.clone()));
        item.insert("value".to_string(), AttributeValue::B(aws_sdk_dynamodb::primitives::Blob::new(vss_record.value.clone())));
        item.insert("version".to_string(), AttributeValue::N(INITIAL_RECORD_VERSION.to_string()));
        item.insert("created_at".to_string(), AttributeValue::S(vss_record.created_at.clone()));
        item.insert("last_updated_at".to_string(), AttributeValue::S(vss_record.last_updated_at.clone()));

        let result = self
            .client
            .put_item()
            .table_name(&self.table_name)
            .set_item(Some(item))
            .send()
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(VssError::InvalidRequestError(format!("DynamoDB error: {}", e))),
        }
    }

    /// Executes put operation based on version logic
    async fn execute_put_object_query(&self, vss_record: &VssDbRecord) -> Result<(), VssError> {
        if vss_record.version == -1 {
            self.execute_non_conditional_upsert(vss_record).await
        } else if vss_record.version == 0 {
            self.execute_conditional_insert(vss_record).await
        } else {
            self.execute_conditional_update(vss_record).await
        }
    }

    /// Executes a conditional delete operation
    async fn execute_conditional_delete(&self, vss_record: &VssDbRecord) -> Result<bool, VssError> {
        let result = self
            .client
            .delete_item()
            .table_name(&self.table_name)
            .key("namespace", AttributeValue::S(vss_record.namespace.clone()))
            .key("key", AttributeValue::S(vss_record.key.clone()))
            .condition_expression("#version = :expected_version")
            .expression_attribute_names("#version", "version")
            .expression_attribute_values(":expected_version", AttributeValue::N(vss_record.version.to_string()))
            .return_values(ReturnValue::AllOld)
            .send()
            .await;

        match result {
            Ok(output) => Ok(output.attributes().is_some()),
            Err(err) => {
                if let Some(DeleteItemError::ConditionalCheckFailedException(_)) = err.as_service_error() {
                    Ok(false)
                } else {
                    Err(VssError::InvalidRequestError(format!("DynamoDB error: {}", err)))
                }
            }
        }
    }

    /// Executes a non-conditional delete operation
    async fn execute_non_conditional_delete(&self, vss_record: &VssDbRecord) -> Result<bool, VssError> {
        let result = self
            .client
            .delete_item()
            .table_name(&self.table_name)
            .key("namespace", AttributeValue::S(vss_record.namespace.clone()))
            .key("key", AttributeValue::S(vss_record.key.clone()))
            .return_values(ReturnValue::AllOld)
            .send()
            .await;

        match result {
            Ok(output) => Ok(output.attributes().is_some()),
            Err(e) => Err(VssError::InvalidRequestError(format!("DynamoDB error: {}", e))),
        }
    }

    /// Executes delete operation based on version logic
    async fn execute_delete_object_query(&self, vss_record: &VssDbRecord) -> Result<bool, VssError> {
        if vss_record.version == -1 {
            self.execute_non_conditional_delete(vss_record).await
        } else {
            self.execute_conditional_delete(vss_record).await
        }
    }
}

#[async_trait]
impl KvStore for DynamoDbBackendImpl {
    async fn get(
        &self, user_token: String, request: GetObjectRequest,
    ) -> Result<GetObjectResponse, VssError> {
        let namespace = self.create_namespace(&user_token, &request.store_id);

        let result = self
            .client
            .get_item()
            .table_name(&self.table_name)
            .key("namespace", AttributeValue::S(namespace))
            .key("key", AttributeValue::S(request.key.clone()))
            .send()
            .await
            .map_err(|e| VssError::InvalidRequestError(format!("DynamoDB error: {}", e)))?;

        if let Some(item) = result.item() {
            let key_value = self.item_to_key_value(item)?;
            Ok(GetObjectResponse { value: Some(key_value) })
        } else if request.key == GLOBAL_VERSION_KEY {
            // Return default global version if not found
            let key_value = KeyValue {
                key: GLOBAL_VERSION_KEY.to_string(),
                value: Bytes::new(),
                version: 0,
            };
            Ok(GetObjectResponse { value: Some(key_value) })
        } else {
            Err(VssError::NoSuchKeyError("Requested key not found.".to_string()))
        }
    }

    async fn put(
        &self, user_token: String, request: PutObjectRequest,
    ) -> Result<PutObjectResponse, VssError> {
        if request.transaction_items.len() + request.delete_items.len() > MAX_PUT_REQUEST_ITEM_COUNT {
            return Err(VssError::InvalidRequestError(format!(
                "Number of write items per request should be less than equal to {}",
                MAX_PUT_REQUEST_ITEM_COUNT
            )));
        }

        let mut vss_put_records: Vec<VssDbRecord> = request
            .transaction_items
            .into_iter()
            .map(|kv| self.build_vss_record(user_token.clone(), request.store_id.clone(), kv))
            .collect();

        let vss_delete_records: Vec<VssDbRecord> = request
            .delete_items
            .into_iter()
            .map(|kv| self.build_vss_record(user_token.clone(), request.store_id.clone(), kv))
            .collect();

        if let Some(global_version) = request.global_version {
            let global_version_record = self.build_vss_record(
                user_token,
                request.store_id,
                KeyValue {
                    key: GLOBAL_VERSION_KEY.to_string(),
                    value: Bytes::new(),
                    version: global_version,
                },
            );
            vss_put_records.push(global_version_record);
        }

        // Process put operations
        for vss_record in &vss_put_records {
            self.execute_put_object_query(vss_record).await?;
        }

        // Process delete operations
        for vss_record in &vss_delete_records {
            let deleted = self.execute_delete_object_query(vss_record).await?;
            if !deleted && vss_record.version != -1 {
                return Err(VssError::ConflictError(
                    "Transaction could not be completed due to a possible conflict".to_string(),
                ));
            }
        }

        Ok(PutObjectResponse {})
    }

    async fn delete(
        &self, user_token: String, request: DeleteObjectRequest,
    ) -> Result<DeleteObjectResponse, VssError> {
        let key_value = request.key_value.ok_or_else(|| {
            VssError::InvalidRequestError("key_value missing in DeleteObjectRequest".to_string())
        })?;

        let vss_record = self.build_vss_record(user_token, request.store_id, key_value);
        self.execute_delete_object_query(&vss_record).await?;

        Ok(DeleteObjectResponse {})
    }

    async fn list_key_versions(
        &self, user_token: String, request: ListKeyVersionsRequest,
    ) -> Result<ListKeyVersionsResponse, VssError> {
        let namespace = self.create_namespace(&user_token, &request.store_id);
        let page_size = request.page_size.unwrap_or(i32::MAX);
        let limit = std::cmp::min(page_size, LIST_KEY_VERSIONS_MAX_PAGE_SIZE);

        // Get global version for first page only
        let mut global_version = None;
        if request.page_token.is_none() {
            let get_global_version_request = GetObjectRequest {
                store_id: request.store_id.clone(),
                key: GLOBAL_VERSION_KEY.to_string(),
            };
            let get_response = self.get(user_token.clone(), get_global_version_request).await?;
            global_version = Some(get_response.value.unwrap().version);
        }

        // Build query parameters
        let mut query_builder = self
            .client
            .query()
            .table_name(&self.table_name)
            .key_condition_expression("#namespace = :namespace")
            .expression_attribute_names("#namespace", "namespace")
            .expression_attribute_values(":namespace", AttributeValue::S(namespace))
            .limit(limit + 1); // Get one extra to check if there are more items

        // Add key prefix filter if provided
        if let Some(key_prefix) = &request.key_prefix {
            query_builder = query_builder
                .filter_expression("begins_with(#key, :key_prefix)")
                .expression_attribute_names("#key", "key")
                .expression_attribute_values(":key_prefix", AttributeValue::S(key_prefix.clone()));
        }

        // Add pagination if page token exists
        if let Some(page_token) = &request.page_token {
            let mut exclusive_start_key = HashMap::new();
            exclusive_start_key.insert("namespace".to_string(), AttributeValue::S(self.create_namespace(&user_token, &request.store_id)));
            exclusive_start_key.insert("key".to_string(), AttributeValue::S(page_token.clone()));
            query_builder = query_builder.set_exclusive_start_key(Some(exclusive_start_key));
        }

        let result = query_builder
            .send()
            .await
            .map_err(|e| VssError::InvalidRequestError(format!("DynamoDB error: {}", e)))?;

        let items = result.items();
        let mut key_versions: Vec<KeyValue> = Vec::new();
        let mut next_page_token = None;

        for (i, item) in items.iter().enumerate() {
            if i >= limit as usize {
                // We have more items, set next page token
                if let Some(key_attr) = item.get(&String::from("key")) {
                    if let Ok(key) = key_attr.as_s() {
                        next_page_token = Some(key.clone());
                    }
                }
                break;
            }

            let key_value = self.item_to_key_value(item)?;
            if key_value.key != GLOBAL_VERSION_KEY {
                key_versions.push(KeyValue {
                    key: key_value.key,
                    value: Bytes::new(), // Don't return value in list operations
                    version: key_value.version,
                });
            }
        }

        if next_page_token.is_none() && !key_versions.is_empty() {
            next_page_token = Some("".to_string());
        }

        Ok(ListKeyVersionsResponse {
            key_versions,
            next_page_token,
            global_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::define_kv_store_tests;

    // Integration tests would require AWS credentials and a real DynamoDB table
    // For now, we'll leave this commented out
    // define_kv_store_tests!(
    //     DynamoDbKvStoreTest,
    //     DynamoDbBackendImpl,
    //     DynamoDbBackendImpl::new(
    //         create_dynamodb_client().await.unwrap(),
    //         "test-table".to_string()
    //     )
    // );
} 