//! Reading a Linear workspace through its GraphQL API with a personal API
//! key the app keeps in the Keychain. Each connection is paged to its end
//! and the answers are joined into the shape [`super::linear::api`] reads,
//! the same one a saved API answer has.

use super::batch::Batch;
use serde_json::{json, Map, Value};
use std::time::Duration;

pub(crate) const ENDPOINT: &str = "https://api.linear.app/graphql";

const PAGE: &str = "pageInfo { hasNextPage endCursor }";

/// One top-level connection: the key it is kept under and the fields read
/// for each node.
struct Connection {
    key: &'static str,
    page_size: u32,
    arguments: &'static str,
    fields: &'static str,
}

const CONNECTIONS: [Connection; 7] = [
    Connection {
        key: "issueLabels",
        page_size: 100,
        arguments: "",
        fields: "id name color isGroup parent { name }",
    },
    Connection {
        key: "initiatives",
        page_size: 50,
        arguments: "",
        fields: "id name description status targetDate projects(first: 100) { nodes { id } }",
    },
    Connection {
        key: "projects",
        page_size: 50,
        arguments: ", includeArchived: true",
        fields: "id name description content priority startDate targetDate state status { type } initiatives(first: 5) { nodes { id } } projectMilestones(first: 50) { nodes { id name description targetDate } }",
    },
    Connection {
        key: "cycles",
        page_size: 100,
        arguments: "",
        fields: "id number name startsAt endsAt",
    },
    Connection {
        key: "issues",
        page_size: 25,
        arguments: ", includeArchived: true",
        fields: "id identifier number title description priority estimate dueDate createdAt updatedAt completedAt canceledAt archivedAt url state { name type } assignee { name displayName } project { id } projectMilestone { id } cycle { id } parent { id } labels(first: 20) { nodes { id name color parent { name } } } comments(first: 100) { nodes { id body createdAt parent { id } user { name displayName } botActor { name } externalUser { name } } }",
    },
    Connection {
        key: "documents",
        page_size: 50,
        arguments: "",
        fields: "id title content createdAt project { id }",
    },
    Connection {
        key: "customViews",
        page_size: 50,
        arguments: "",
        fields: "id name modelName filterData",
    },
];

fn refused(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => "Linear turned the key down. Make a new personal API key in Linear under Settings › Security & access, and paste it again.".to_owned(),
        429 => "Linear asked Sona to slow down. Try again in a minute.".to_owned(),
        _ => format!("Linear answered with an error ({status}). Try again later."),
    }
}

async fn ask(
    client: &reqwest::Client,
    endpoint: &str,
    key: &str,
    query: &str,
    variables: Value,
) -> Result<Value, String> {
    let response = client
        .post(endpoint)
        .header("Authorization", key.trim())
        .json(&json!({ "query": query, "variables": variables }))
        .send()
        .await
        .map_err(|_| {
            "Linear could not be reached. Check the connection and try again.".to_owned()
        })?;
    let status = response.status();
    let body: Value = response.json().await.map_err(|_| {
        if status.is_success() {
            "Linear's answer could not be read.".to_owned()
        } else {
            refused(status)
        }
    })?;
    if let Some(message) = body
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|errors| errors.first())
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        return Err(if status.is_success() {
            format!("Linear could not answer: {message}")
        } else {
            refused(status)
        });
    }
    if !status.is_success() {
        return Err(refused(status));
    }
    body.get("data")
        .cloned()
        .ok_or_else(|| "Linear's answer could not be read.".to_owned())
}

/// Every node of one connection, page by page.
async fn every(
    client: &reqwest::Client,
    endpoint: &str,
    key: &str,
    connection: &Connection,
) -> Result<Vec<Value>, String> {
    let query = format!(
        "query Page($after: String) {{ {}(first: {}, after: $after{}) {{ nodes {{ {} }} {PAGE} }} }}",
        connection.key, connection.page_size, connection.arguments, connection.fields
    );
    let mut nodes = Vec::new();
    let mut after = Value::Null;
    loop {
        let data = ask(client, endpoint, key, &query, json!({ "after": after })).await?;
        let page = data
            .get(connection.key)
            .ok_or_else(|| "Linear's answer could not be read.".to_owned())?;
        if let Some(found) = page.get("nodes").and_then(Value::as_array) {
            nodes.extend(found.iter().cloned());
        }
        let info = page.get("pageInfo");
        let more = info
            .and_then(|info| info.get("hasNextPage"))
            .and_then(Value::as_bool)
            == Some(true);
        let cursor = info
            .and_then(|info| info.get("endCursor"))
            .cloned()
            .unwrap_or(Value::Null);
        if !more || cursor.is_null() {
            return Ok(nodes);
        }
        after = cursor;
    }
}

/// Reads the workspace the key belongs to: the person it is, labels,
/// initiatives, projects with milestones, cycles, issues with comments,
/// documents and saved views.
pub(crate) async fn fetch(endpoint: &str, key: &str) -> Result<Batch, String> {
    if key.trim().is_empty() {
        return Err("Paste a Linear personal API key first.".to_owned());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| {
            "Linear could not be reached. Check the connection and try again.".to_owned()
        })?;
    let mut root = Map::new();
    let viewer = ask(
        &client,
        endpoint,
        key,
        "query { viewer { name displayName email } }",
        Value::Null,
    )
    .await?;
    if let Some(viewer) = viewer.get("viewer") {
        root.insert("viewer".to_owned(), viewer.clone());
    }
    for connection in &CONNECTIONS {
        let nodes = every(&client, endpoint, key, connection).await?;
        root.insert(connection.key.to_owned(), Value::Array(nodes));
    }
    Ok(super::linear::api(&root))
}
