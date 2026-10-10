//! `openapi.yaml` and the router agree: every documented operation is routed
//! and answers an unauthenticated call the way its security says.

use http::StatusCode;
use serde_json::Value;

use super::{Harness, code};

const API_PREFIX: &str = "/api/v2";
const METHODS: [&str; 5] = ["get", "put", "post", "patch", "delete"];

struct Documented {
    method: String,
    uri: String,
    security: Option<Value>,
}

fn documented() -> anyhow::Result<Vec<Documented>> {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/openapi.yaml"))?;
    let document: Value = serde_yml::from_str(&source)?;
    let paths = document["paths"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("openapi.yaml has no paths"))?;
    let mut operations = Vec::new();
    for (path, item) in paths {
        // Path-level servers move an operation off /api/v2 to the service root.
        let at_root = item["servers"].as_array().is_some_and(|servers| {
            servers.iter().any(|server| {
                server["url"]
                    .as_str()
                    .is_some_and(|url| !url.ends_with(API_PREFIX))
            })
        });
        let concrete = path
            .replace("{schedule_id}", "0199d8d5-2f62-7eb7-81c5-1e456778f8de")
            .replace("{subscription_id}", "0199d8d5-2f62-7eb7-81c5-1e456778f8de")
            .replace("{id}", "0199d8d5-2f62-7eb7-81c5-1e456778f8de")
            .replace("{viewer}", "c:ada")
            .replace("{account}", "c:ada");
        for method in METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };
            operations.push(Documented {
                method: method.to_ascii_uppercase(),
                uri: if at_root {
                    concrete.clone()
                } else {
                    format!("{API_PREFIX}{concrete}")
                },
                security: operation.get("security").cloned(),
            });
        }
    }
    Ok(operations)
}

#[tokio::test]
async fn every_documented_operation_is_routed_and_guarded() -> anyhow::Result<()> {
    let harness = Harness::new().await?;
    let operations = documented()?;
    assert!(
        operations.len() >= 30,
        "found {} operations",
        operations.len()
    );
    for operation in operations {
        let (status, body) = harness
            .send(&operation.method, &operation.uri, None, None, &[])
            .await?;
        let label = format!("{} {}", operation.method, operation.uri);
        assert!(
            !matches!(code(&body), "not_found" | "method_not_allowed"),
            "{label} is documented but not routed: {status} {body}"
        );
        let public = operation
            .security
            .as_ref()
            .is_some_and(|security| security.as_array().is_some_and(Vec::is_empty));
        let test_key_only = operation
            .security
            .as_ref()
            .is_some_and(|security| security.to_string().contains("testKeyAuth"));
        if test_key_only {
            assert_eq!(
                (status, code(&body)),
                (StatusCode::BAD_REQUEST, "test_environment_required"),
                "{label}"
            );
        } else if !public {
            assert_eq!(
                (status, code(&body)),
                (StatusCode::UNAUTHORIZED, "unauthenticated"),
                "{label} must require a credential"
            );
        }
    }
    Ok(())
}
