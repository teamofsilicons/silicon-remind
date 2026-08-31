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
fn contract_exposes_exactly_the_six_documented_operations() -> Result<()> {
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
        ("get", "/schedules", "listSchedules"),
        ("post", "/schedules", "createSchedule"),
        ("get", "/schedules/{schedule_id}", "getSchedule"),
        ("patch", "/schedules/{schedule_id}", "updateSchedule"),
        ("delete", "/schedules/{schedule_id}", "deleteSchedule"),
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

#[test]
fn each_operation_has_expected_auth_tenant_and_mutation_contract() -> Result<()> {
    let document = load_contract()?;
    let bearer_security = json!([{ "bearerAuth": [] }]);
    let operations = [
        (
            "/schedules",
            "get",
            [
                "#/components/parameters/OrgId",
                "#/components/parameters/Cursor",
                "#/components/parameters/Limit",
            ]
            .as_slice(),
        ),
        (
            "/schedules",
            "post",
            [
                "#/components/parameters/OrgId",
                "#/components/parameters/IdempotencyKey",
            ]
            .as_slice(),
        ),
        (
            "/schedules/{schedule_id}",
            "get",
            [
                "#/components/parameters/OrgId",
                "#/components/parameters/ScheduleId",
            ]
            .as_slice(),
        ),
        (
            "/schedules/{schedule_id}",
            "patch",
            [
                "#/components/parameters/OrgId",
                "#/components/parameters/ScheduleId",
                "#/components/parameters/IdempotencyKey",
            ]
            .as_slice(),
        ),
        (
            "/schedules/{schedule_id}",
            "delete",
            [
                "#/components/parameters/OrgId",
                "#/components/parameters/ScheduleId",
            ]
            .as_slice(),
        ),
        (
            "/schedules/{schedule_id}/executions",
            "get",
            [
                "#/components/parameters/OrgId",
                "#/components/parameters/ScheduleId",
                "#/components/parameters/Cursor",
                "#/components/parameters/Limit",
            ]
            .as_slice(),
        ),
    ];
    for (path, method, expected_references) in operations {
        let operation = operation(&document, path, method)?;
        let effective_security = operation
            .get("security")
            .or_else(|| document.get("security"));
        ensure!(
            effective_security == Some(&bearer_security),
            "{method} {path} must require bearer authentication"
        );
        let references = effective_parameter_refs(&document, path, method)?;
        let expected_references = expected_references
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();
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

    let create = object_at(&document, "/components/schemas/ScheduleCreate")?;
    ensure!(
        string_set(
            create
                .get("required")
                .context("ScheduleCreate.required is missing")?,
            "ScheduleCreate.required",
        )? == BTreeSet::from(["text".to_owned(), "timezone".to_owned()]),
        "ScheduleCreate must require text and timezone"
    );
    let alternatives = create
        .get("oneOf")
        .and_then(Value::as_array)
        .context("ScheduleCreate.oneOf is missing")?;
    ensure!(
        alternatives.len() == 2,
        "ScheduleCreate must have exactly two timing alternatives"
    );
    let timing_requirements = alternatives
        .iter()
        .map(|alternative| {
            string_set(
                alternative
                    .get("required")
                    .context("timing alternative must have required")?,
                "timing alternative required",
            )
        })
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        timing_requirements
            == BTreeSet::from([
                BTreeSet::from(["run_at".to_owned()]),
                BTreeSet::from(["cron".to_owned()]),
            ]),
        "ScheduleCreate must require exactly one timing representation"
    );
    ensure!(
        document.pointer("/components/schemas/ScheduleCreate/properties/text/minLength")
            == Some(&json!(1))
            && document.pointer("/components/schemas/ScheduleCreate/properties/text/maxLength")
                == Some(&json!(100_000))
            && document.pointer("/components/schemas/ScheduleCreate/properties/run_at/format")
                == Some(&Value::String("date-time".to_owned())),
        "ScheduleCreate text or run_at constraints changed"
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
        document.pointer("/paths/~1schedules/post/responses/409/$ref")
            == Some(&Value::String(
                "#/components/responses/WebhookNotConfigured".to_owned()
            ))
            && document.pointer(
                "/components/responses/WebhookNotConfigured/content/application~1json/example/error/code",
            ) == Some(&Value::String("webhook_not_configured".to_owned()))
            && document.pointer(
                "/components/responses/WebhookNotConfigured/content/application~1json/example/error/message",
            ) == Some(&Value::String("Set the webhook url first.".to_owned())),
        "missing-webhook response contract changed"
    );
    ensure!(
        document.pointer("/components/parameters/Limit/schema/minimum") == Some(&json!(1))
            && document.pointer("/components/parameters/Limit/schema/maximum") == Some(&json!(100))
            && document.pointer("/components/parameters/Limit/schema/default") == Some(&json!(20)),
        "pagination limit bounds changed"
    );
    Ok(())
}
