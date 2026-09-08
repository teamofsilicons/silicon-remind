//! Machine-readable checks for the committed public API contract.

use std::{collections::BTreeSet, fs, path::PathBuf};

use anyhow::{Context as _, Result, ensure};
use serde_json::{Map, Value, json};

const HTTP_METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

fn load_contract() -> Result<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("openapi.yaml");
    let source =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_yml::from_str(&source).context("openapi.yaml must be valid YAML")
}

fn object_at<'a>(document: &'a Value, pointer: &str) -> Result<&'a Map<String, Value>> {
    document
        .pointer(pointer)
        .with_context(|| format!("missing OpenAPI value at {pointer}"))?
        .as_object()
        .with_context(|| format!("OpenAPI value at {pointer} must be an object"))
}

fn string_set(value: &Value, description: &str) -> Result<BTreeSet<String>> {
    value
        .as_array()
        .with_context(|| format!("{description} must be an array"))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .with_context(|| format!("{description} entries must be strings"))
        })
        .collect()
}

fn operation<'a>(document: &'a Value, path: &str, method: &str) -> Result<&'a Map<String, Value>> {
    let paths = object_at(document, "/paths")?;
    let path_item = paths
        .get(path)
        .with_context(|| format!("missing path {path}"))?
        .as_object()
        .with_context(|| format!("path item {path} must be an object"))?;
    path_item
        .get(method)
        .with_context(|| format!("missing {method} operation at {path}"))?
        .as_object()
        .with_context(|| format!("{method} operation at {path} must be an object"))
}

fn effective_parameter_refs(
    document: &Value,
    path: &str,
    method: &str,
) -> Result<BTreeSet<String>> {
    let paths = object_at(document, "/paths")?;
    let path_item = paths
        .get(path)
        .and_then(Value::as_object)
        .with_context(|| format!("missing path item {path}"))?;
    let operation = operation(document, path, method)?;
    let mut references = BTreeSet::new();

    for owner in [path_item, operation] {
        let Some(parameters) = owner.get("parameters") else {
            continue;
        };
        for parameter in parameters
            .as_array()
            .context("parameters must be an array")?
        {
            if let Some(reference) = parameter.get("$ref").and_then(Value::as_str) {
                references.insert(reference.to_owned());
            }
        }
    }
    Ok(references)
}

fn validate_local_refs(value: &Value, root: &Value, location: &str) -> Result<usize> {
    match value {
        Value::Object(object) => {
            let mut count = 0;
            if let Some(reference) = object.get("$ref").and_then(Value::as_str)
                && let Some(pointer) = reference.strip_prefix('#')
            {
                ensure!(
                    pointer.starts_with('/'),
                    "local reference at {location} is not a JSON pointer: {reference}"
                );
                ensure!(
                    root.pointer(pointer).is_some(),
                    "unresolved local reference at {location}: {reference}"
                );
                count += 1;
            }
            for (key, child) in object {
                count += validate_local_refs(child, root, &format!("{location}/{key}"))?;
            }
            Ok(count)
        }
        Value::Array(array) => {
            let mut count = 0;
            for (index, child) in array.iter().enumerate() {
                count += validate_local_refs(child, root, &format!("{location}/{index}"))?;
            }
            Ok(count)
        }
        _ => Ok(0),
    }
}

#[test]
fn contract_is_openapi_31_and_every_local_reference_resolves() -> Result<()> {
    let document = load_contract()?;
    ensure!(
        document.get("openapi") == Some(&Value::String("3.1.0".to_owned())),
        "the public contract must remain OpenAPI 3.1.0"
    );

    let reference_count = validate_local_refs(&document, &document, "#")?;
    ensure!(
        reference_count > 0,
        "contract unexpectedly contains no local references"
    );
    Ok(())
}

#[test]
fn contract_exposes_exactly_the_documented_public_operations() -> Result<()> {
    let document = load_contract()?;
    let paths = object_at(&document, "/paths")?;
    let mut actual = BTreeSet::new();
    for (path, path_item) in paths {
        let path_item = path_item
            .as_object()
            .with_context(|| format!("path item {path} must be an object"))?;
        for method in HTTP_METHODS {
            let Some(operation) = path_item.get(method) else {
                continue;
            };
            let operation_id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .with_context(|| format!("{method} {path} needs an operationId"))?;
            actual.insert((method.to_owned(), path.clone(), operation_id.to_owned()));
        }
    }

    let expected = [
        ("post", "/auth/login", "authLogin"),
        ("post", "/auth/refresh", "authRefresh"),
        ("post", "/auth/logout", "authLogout"),
        ("get", "/auth/me", "getIdentity"),
        ("get", "/auth/organizations", "listAuthorizedOrganizations"),
        ("get", "/webhook", "getWebhook"),
        ("put", "/webhook", "configureWebhook"),
        ("delete", "/webhook", "disableWebhook"),
        ("get", "/webhooks", "listWebhooks"),
        ("post", "/webhooks", "subscribeWebhook"),
        (
            "delete",
            "/webhooks/{subscription_id}",
            "unsubscribeWebhook",
        ),
        ("get", "/silicons", "listSilicons"),
        ("get", "/test-environments", "listTestEnvironments"),
        ("post", "/test-environments", "createTestEnvironment"),
        ("get", "/test-environments/{id}", "getTestEnvironment"),
        ("delete", "/test-environments/{id}", "deleteTestEnvironment"),
        ("get", "/test-environments/{id}/key", "getEnvironmentKey"),
        (
            "post",
            "/test-environments/{id}/key-rotations",
            "rotateEnvironmentKey",
        ),
        (
            "post",
            "/test-environments/{id}/restorations",
            "restoreEnvironment",
        ),
        ("get", "/testing-environment", "currentEnvironment"),
        ("post", "/testing-environment/cleanings", "cleanEnvironment"),
        ("put", "/testing-environment/iam", "configureEnvironmentIam"),
        ("get", "/health/live", "healthLive"),
        ("get", "/health/ready", "healthReady"),
        ("post", "/webhook/", "receiveIamWebhook"),
        ("get", "/schedules", "listSchedules"),
        ("patch", "/schedules", "updateScheduleStatuses"),
        ("post", "/schedules", "createSchedule"),
        ("get", "/schedules/{schedule_id}", "getSchedule"),
        ("patch", "/schedules/{schedule_id}", "updateSchedule"),
        ("delete", "/schedules/{schedule_id}", "archiveSchedule"),
        (
            "get",
            "/schedules/{schedule_id}/executions",
            "listScheduleExecutions",
        ),
    ]
    .into_iter()
    .map(|(method, path, operation_id)| {
        (method.to_owned(), path.to_owned(), operation_id.to_owned())
    })
    .collect::<BTreeSet<_>>();

    ensure!(
        actual == expected,
        "public operation set changed: {actual:#?}"
    );
    Ok(())
}

#[test]
fn authentication_and_shared_parameter_definitions_are_stable() -> Result<()> {
    let document = load_contract()?;
    let bearer_security = json!([{ "bearerAuth": [] }]);
    ensure!(
        document.get("security") == Some(&bearer_security),
        "all public operations must inherit IAM bearer authentication"
    );
    ensure!(
        document.pointer("/components/securitySchemes/bearerAuth/type")
            == Some(&Value::String("http".to_owned()))
            && document.pointer("/components/securitySchemes/bearerAuth/scheme")
                == Some(&Value::String("bearer".to_owned())),
        "bearerAuth must remain an HTTP bearer scheme"
    );

    let org = object_at(&document, "/components/parameters/OrgId")?;
    ensure!(
        org.get("name") == Some(&Value::String("X-Org-ID".to_owned()))
            && org.get("in") == Some(&Value::String("header".to_owned()))
            && org.get("required") == Some(&Value::Bool(true))
            && document.pointer("/components/parameters/OrgId/schema/$ref")
                == Some(&Value::String("#/components/schemas/OrgId".to_owned()))
            && document.pointer("/components/schemas/OrgId/pattern")
                == Some(&Value::String("^[a-z0-9_-]{3,50}$".to_owned())),
        "OrgId must be the required X-Org-ID header"
    );
    let schedule_id = object_at(&document, "/components/parameters/ScheduleId")?;
    ensure!(
        schedule_id.get("name") == Some(&Value::String("schedule_id".to_owned()))
            && schedule_id.get("in") == Some(&Value::String("path".to_owned()))
            && schedule_id.get("required") == Some(&Value::Bool(true))
            && document.pointer("/components/parameters/ScheduleId/schema/type")
                == Some(&Value::String("string".to_owned()))
            && document.pointer("/components/parameters/ScheduleId/schema/format")
                == Some(&Value::String("uuid".to_owned())),
        "ScheduleId must be a required UUID path parameter"
    );
    let idempotency = object_at(&document, "/components/parameters/IdempotencyKey")?;
    ensure!(
        idempotency.get("name") == Some(&Value::String("Idempotency-Key".to_owned()))
            && idempotency.get("in") == Some(&Value::String("header".to_owned()))
            && idempotency.get("required") == Some(&Value::Bool(true))
            && document.pointer("/components/parameters/IdempotencyKey/schema/type")
                == Some(&Value::String("string".to_owned()))
            && document.pointer("/components/parameters/IdempotencyKey/schema/minLength")
                == Some(&json!(8))
            && document.pointer("/components/parameters/IdempotencyKey/schema/maxLength")
                == Some(&json!(255)),
        "Idempotency-Key requirements changed"
    );
    Ok(())
}

const OPERATION_PARAMETERS: &[(&str, &str, &[&str])] = &[
    (
        "/schedules",
        "get",
        &[
            "#/components/parameters/OrgId",
            "#/components/parameters/Cursor",
            "#/components/parameters/Limit",
        ],
    ),
    (
        "/schedules",
        "patch",
        &[
            "#/components/parameters/OrgId",
            "#/components/parameters/IdempotencyKey",
        ],
    ),
    (
        "/schedules",
        "post",
        &[
            "#/components/parameters/OrgId",
            "#/components/parameters/IdempotencyKey",
        ],
    ),
    (
        "/schedules/{schedule_id}",
        "get",
        &[
            "#/components/parameters/OrgId",
            "#/components/parameters/ScheduleId",
        ],
    ),
    (
        "/schedules/{schedule_id}",
        "patch",
        &[
            "#/components/parameters/OrgId",
            "#/components/parameters/ScheduleId",
            "#/components/parameters/IdempotencyKey",
        ],
    ),
    (
        "/schedules/{schedule_id}",
        "delete",
        &[
            "#/components/parameters/OrgId",
            "#/components/parameters/ScheduleId",
        ],
    ),
    (
        "/schedules/{schedule_id}/executions",
        "get",
        &[
            "#/components/parameters/OrgId",
            "#/components/parameters/ScheduleId",
            "#/components/parameters/Cursor",
            "#/components/parameters/Limit",
        ],
    ),
];

#[test]
fn each_operation_has_expected_auth_tenant_and_mutation_contract() -> Result<()> {
    let document = load_contract()?;
    let bearer_security = json!([{ "bearerAuth": [] }]);
    for &(path, method, expected_references) in OPERATION_PARAMETERS {
        let operation = operation(&document, path, method)?;
        let effective_security = operation
            .get("security")
            .or_else(|| document.get("security"));
        ensure!(
            effective_security == Some(&bearer_security),
            "{method} {path} must require bearer authentication"
        );
        let references = effective_parameter_refs(&document, path, method)?;
        let mut expected_references = expected_references
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();
        expected_references.insert("#/components/parameters/TestKey".into());
        ensure!(
            references == expected_references,
            "{method} {path} shared parameter requirements changed: {references:#?}"
        );
        let default_error_ref = operation
            .get("responses")
            .and_then(Value::as_object)
            .and_then(|responses| responses.get("default"))
            .and_then(Value::as_object)
            .and_then(|response| response.get("$ref"))
            .and_then(Value::as_str);
        ensure!(
            default_error_ref == Some("#/components/responses/Error"),
            "{method} {path} must use the shared default error response"
        );
    }
    Ok(())
}

#[test]
fn core_schedule_schemas_are_stable() -> Result<()> {
    let document = load_contract()?;
    assert_schedule_enums(&document)?;

    let create = object_at(&document, "/components/schemas/ScheduleCreate")?;
    ensure!(
        string_set(
            create
                .get("required")
                .context("ScheduleCreate.required is missing")?,
            "ScheduleCreate.required",
        )? == BTreeSet::from(["cron".to_owned(), "kind".to_owned(), "text".to_owned()]),
        "ScheduleCreate must require text, kind, and cron"
    );
    ensure!(
        create.get("oneOf").is_none(),
        "ScheduleCreate must use explicit kind plus cron, not timing alternatives"
    );
    ensure!(
        document.pointer("/components/schemas/ScheduleCreate/properties/text/minLength")
            == Some(&json!(1))
            && document.pointer("/components/schemas/ScheduleCreate/properties/text/maxLength")
                == Some(&json!(100_000))
            && document.pointer("/components/schemas/ScheduleCreate/properties/kind/$ref")
                == Some(&Value::String(
                    "#/components/schemas/ScheduleKind".to_owned(),
                ))
            && document.pointer("/components/schemas/ScheduleCreate/properties/cron/type")
                == Some(&Value::String("string".to_owned()))
            && document.pointer("/components/schemas/ScheduleCreate/properties/timezone/default")
                == Some(&Value::String("UTC".to_owned()))
            && document
                .pointer("/components/schemas/ScheduleCreate/properties/run_at")
                .is_none(),
        "ScheduleCreate cron-only timing constraints changed"
    );

    let schedule_required = document
        .pointer("/components/schemas/Schedule/allOf/1/required")
        .context("Schedule required fields are missing")?;
    ensure!(
        string_set(schedule_required, "Schedule required fields")?
            == BTreeSet::from([
                "id".to_owned(),
                "org_id".to_owned(),
                "silicon_id".to_owned(),
                "status".to_owned(),
                "section".to_owned(),
                "next_run_at".to_owned(),
                "archived_at".to_owned(),
                "purge_after".to_owned(),
                "timezone".to_owned(),
                "created_at".to_owned(),
                "updated_at".to_owned(),
            ]),
        "Schedule required fields changed"
    );
    ensure!(
        document.pointer("/components/schemas/Schedule/allOf/0/$ref")
            == Some(&Value::String(
                "#/components/schemas/ScheduleCreate".to_owned(),
            )),
        "Schedule must compose ScheduleCreate"
    );
    ensure!(
        document.pointer("/paths/~1schedules/post/requestBody/content/application~1json/schema/$ref")
            == Some(&Value::String(
                "#/components/schemas/ScheduleCreate".to_owned(),
            ))
            && document.pointer(
                "/paths/~1schedules~1{schedule_id}/patch/requestBody/content/application~1json/schema/minProperties",
            ) == Some(&json!(1)),
        "create and patch request schema contracts changed"
    );
    assert_archive_contract(&document)?;
    assert_bulk_status_update_contract(&document)?;
    assert_patch_uses_cron_timing(&document)?;
    Ok(())
}

fn assert_schedule_enums(document: &Value) -> Result<()> {
    let statuses = document
        .pointer("/components/schemas/ScheduleStatus/enum")
        .context("ScheduleStatus enum is missing")?;
    ensure!(
        string_set(statuses, "ScheduleStatus enum")?
            == BTreeSet::from([
                "active".to_owned(),
                "paused".to_owned(),
                "completed".to_owned(),
            ]),
        "ScheduleStatus values changed"
    );
    let mutable_statuses = document
        .pointer("/components/schemas/MutableScheduleStatus/enum")
        .context("MutableScheduleStatus enum is missing")?;
    ensure!(
        string_set(mutable_statuses, "MutableScheduleStatus enum")?
            == BTreeSet::from(["active".to_owned(), "paused".to_owned()]),
        "clients must not set the worker-owned completed status"
    );
    let schedule_kinds = document
        .pointer("/components/schemas/ScheduleKind/enum")
        .context("ScheduleKind enum is missing")?;
    ensure!(
        string_set(schedule_kinds, "ScheduleKind enum")?
            == BTreeSet::from(["one_time".to_owned(), "recurring".to_owned()]),
        "ScheduleKind values changed"
    );
    Ok(())
}

fn assert_bulk_status_update_contract(document: &Value) -> Result<()> {
    ensure!(
        document
            .pointer("/paths/~1schedules/patch/requestBody/content/application~1json/schema/$ref",)
            == Some(&json!("#/components/schemas/ScheduleStatusBatchUpdate"))
            && document.pointer(
                "/paths/~1schedules/patch/responses/200/content/application~1json/schema/$ref",
            ) == Some(&json!(
                "#/components/schemas/ScheduleStatusBatchUpdateResponse"
            )),
        "bulk status operation must use its stable request and response schemas"
    );

    assert_bulk_status_request_contract(document)?;
    assert_bulk_status_response_contract(document)?;
    assert_bulk_status_result_contract(document)
}

fn assert_bulk_status_request_contract(document: &Value) -> Result<()> {
    let request = object_at(document, "/components/schemas/ScheduleStatusBatchUpdate")?;
    ensure!(
        string_set(
            request
                .get("required")
                .context("ScheduleStatusBatchUpdate.required is missing")?,
            "ScheduleStatusBatchUpdate.required",
        )? == BTreeSet::from(["schedule_ids".to_owned(), "status".to_owned()])
            && request.get("additionalProperties") == Some(&Value::Bool(false)),
        "bulk status request must require only its bounded ID set and status"
    );
    let request_properties = object_at(
        document,
        "/components/schemas/ScheduleStatusBatchUpdate/properties",
    )?;
    ensure!(
        request_properties.keys().cloned().collect::<BTreeSet<_>>()
            == BTreeSet::from(["schedule_ids".to_owned(), "status".to_owned()]),
        "bulk status request properties changed"
    );
    let schedule_ids = object_at(
        document,
        "/components/schemas/ScheduleStatusBatchUpdate/properties/schedule_ids",
    )?;
    ensure!(
        schedule_ids.get("type") == Some(&json!("array"))
            && schedule_ids.get("minItems") == Some(&json!(1))
            && schedule_ids.get("maxItems") == Some(&json!(100))
            && schedule_ids.get("uniqueItems") == Some(&Value::Bool(true))
            && document.pointer(
                "/components/schemas/ScheduleStatusBatchUpdate/properties/schedule_ids/items/type",
            ) == Some(&json!("string"))
            && document.pointer(
                "/components/schemas/ScheduleStatusBatchUpdate/properties/schedule_ids/items/format",
            ) == Some(&json!("uuid"))
            && document
                .pointer("/components/schemas/ScheduleStatusBatchUpdate/properties/status/$ref",)
                == Some(&json!("#/components/schemas/MutableScheduleStatus")),
        "bulk status request must contain 1 through 100 unique UUIDs and a mutable status"
    );
    Ok(())
}

fn assert_bulk_status_response_contract(document: &Value) -> Result<()> {
    let response = object_at(
        document,
        "/components/schemas/ScheduleStatusBatchUpdateResponse",
    )?;
    ensure!(
        response.get("required") == Some(&json!(["items"]))
            && response.get("additionalProperties") == Some(&Value::Bool(false))
            && document.pointer(
                "/components/schemas/ScheduleStatusBatchUpdateResponse/properties/items/type",
            ) == Some(&json!("array"))
            && document.pointer(
                "/components/schemas/ScheduleStatusBatchUpdateResponse/properties/items/minItems",
            ) == Some(&json!(1))
            && document.pointer(
                "/components/schemas/ScheduleStatusBatchUpdateResponse/properties/items/maxItems",
            ) == Some(&json!(100))
            && document.pointer(
                "/components/schemas/ScheduleStatusBatchUpdateResponse/properties/items/items/$ref",
            ) == Some(&json!("#/components/schemas/ScheduleStatusResult")),
        "bulk status response must contain one bounded compact result array"
    );
    Ok(())
}

fn assert_bulk_status_result_contract(document: &Value) -> Result<()> {
    let result = object_at(document, "/components/schemas/ScheduleStatusResult")?;
    ensure!(
        string_set(
            result
                .get("required")
                .context("ScheduleStatusResult.required is missing")?,
            "ScheduleStatusResult.required",
        )? == BTreeSet::from([
            "id".to_owned(),
            "next_run_at".to_owned(),
            "status".to_owned(),
            "updated_at".to_owned(),
        ]) && result.get("additionalProperties") == Some(&Value::Bool(false)),
        "compact schedule status result required fields changed"
    );
    let result_properties = object_at(
        document,
        "/components/schemas/ScheduleStatusResult/properties",
    )?;
    ensure!(
        result_properties.keys().cloned().collect::<BTreeSet<_>>()
            == BTreeSet::from([
                "id".to_owned(),
                "next_run_at".to_owned(),
                "status".to_owned(),
                "updated_at".to_owned(),
            ])
            && document.pointer("/components/schemas/ScheduleStatusResult/properties/id/type")
                == Some(&json!("string"))
            && document.pointer("/components/schemas/ScheduleStatusResult/properties/id/format")
                == Some(&json!("uuid"))
            && document.pointer("/components/schemas/ScheduleStatusResult/properties/status/$ref")
                == Some(&json!("#/components/schemas/MutableScheduleStatus"))
            && string_set(
                document
                    .pointer(
                        "/components/schemas/ScheduleStatusResult/properties/next_run_at/type",
                    )
                    .context("ScheduleStatusResult.next_run_at type is missing")?,
                "ScheduleStatusResult.next_run_at type",
            )? == BTreeSet::from(["null".to_owned(), "string".to_owned()])
            && document.pointer(
                "/components/schemas/ScheduleStatusResult/properties/next_run_at/format",
            ) == Some(&json!("date-time"))
            && document.pointer(
                "/components/schemas/ScheduleStatusResult/properties/updated_at/type",
            ) == Some(&json!("string"))
            && document.pointer(
                "/components/schemas/ScheduleStatusResult/properties/updated_at/format",
            ) == Some(&json!("date-time")),
        "compact schedule status result fields changed"
    );
    Ok(())
}

fn assert_archive_contract(document: &Value) -> Result<()> {
    let sections = document
        .pointer("/components/schemas/ScheduleSection/enum")
        .context("ScheduleSection enum is missing")?;
    ensure!(
        string_set(sections, "ScheduleSection enum")?
            == BTreeSet::from(["archived".to_owned(), "current".to_owned()]),
        "ScheduleSection values changed"
    );

    let list_parameters = document
        .pointer("/paths/~1schedules/get/parameters")
        .and_then(Value::as_array)
        .context("list schedule parameters are missing")?;
    let section = list_parameters
        .iter()
        .find(|parameter| parameter.get("name") == Some(&json!("section")))
        .context("list schedules must expose the section parameter")?;
    ensure!(
        section.pointer("/schema/default") == Some(&json!("current"))
            && section.pointer("/schema/allOf/0/$ref")
                == Some(&json!("#/components/schemas/ScheduleSection")),
        "list schedules section must default to current"
    );
    Ok(())
}

fn assert_patch_uses_cron_timing(document: &Value) -> Result<()> {
    let base = "/paths/~1schedules~1{schedule_id}/patch/requestBody/content/\
                application~1json/schema/properties";
    ensure!(
        document.pointer(&format!("{base}/kind/$ref"))
            == Some(&Value::String(
                "#/components/schemas/ScheduleKind".to_owned(),
            ))
            && document.pointer(&format!("{base}/cron/type"))
                == Some(&Value::String("string".to_owned()))
            && document.pointer(&format!("{base}/run_at")).is_none(),
        "PATCH must expose kind plus cron without run_at"
    );
    Ok(())
}

#[test]
fn core_execution_error_and_pagination_schemas_are_stable() -> Result<()> {
    let document = load_contract()?;
    let execution_required = document
        .pointer("/components/schemas/Execution/required")
        .context("Execution.required is missing")?;
    ensure!(
        string_set(execution_required, "Execution.required")?
            == BTreeSet::from([
                "id".to_owned(),
                "schedule_id".to_owned(),
                "scheduled_for".to_owned(),
                "status".to_owned(),
            ]),
        "Execution required fields changed"
    );
    let execution_statuses = document
        .pointer("/components/schemas/Execution/properties/status/enum")
        .context("Execution status enum is missing")?;
    ensure!(
        string_set(execution_statuses, "Execution status enum")?
            == BTreeSet::from([
                "pending".to_owned(),
                "delivered".to_owned(),
                "retrying".to_owned(),
                "failed".to_owned(),
            ]),
        "Execution status values changed"
    );

    ensure!(
        document.pointer("/components/schemas/Error/required") == Some(&json!(["error"]))
            && document.pointer("/components/schemas/Error/properties/error/required")
                == Some(&json!(["code", "message", "request_id"]))
            && document
                .pointer("/components/schemas/Error/properties/error/properties/request_id/type",)
                == Some(&Value::String("string".to_owned())),
        "public Error envelope changed"
    );
    ensure!(
        document.pointer("/components/parameters/Limit/schema/minimum") == Some(&json!(1))
            && document.pointer("/components/parameters/Limit/schema/maximum") == Some(&json!(100))
            && document.pointer("/components/parameters/Limit/schema/default") == Some(&json!(20)),
        "pagination limit bounds changed"
    );
    Ok(())
}
