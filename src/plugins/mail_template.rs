use chrono::{NaiveDateTime, Utc};
use postwarden_plugin_api::{
    HookDecision, MailDraft, Mailbox, ManagementOperation, ManagementResult, Plugin, PluginContext,
    PluginError, Recipient,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::database::Database;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub id: String,
    pub scenario_id: String,
    pub name: String,
    pub enabled: bool,
    pub allow_caller_recipients: bool,
    pub sender_identity_id: String,
    pub subject_template: String,
    pub text_template: Option<String>,
    pub html_template: Option<String>,
    pub variables: Vec<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RenderedTemplate {
    pub sender_identity_id: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub allow_caller_recipients: bool,
    pub subject: String,
    pub text: Option<String>,
    pub html: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    InvalidPlaceholder,
    MissingVariable(String),
    InvalidVariable(String),
}

impl Template {
    fn variables(subject: &str, text: Option<&str>, html: Option<&str>) -> Vec<String> {
        let mut variables = Vec::new();
        for input in std::iter::once(subject).chain(text).chain(html) {
            let mut rest = input;
            while let Some(start) = rest.find("{{") {
                let after = &rest[start + 2..];
                let Some(end) = after.find("}}") else {
                    break;
                };
                let name = &after[..end];
                if valid_identifier(name) && !variables.iter().any(|existing| existing == name) {
                    variables.push(name.to_owned());
                }
                rest = &after[end + 2..];
            }
        }
        variables
    }

    pub fn render(&self, values: &Map<String, Value>) -> Result<RenderedTemplate, RenderError> {
        let subject = render_text(&self.subject_template, values, false)?;
        let text = self
            .text_template
            .as_deref()
            .map(|s| render_text(s, values, false))
            .transpose()?;
        let html = self
            .html_template
            .as_deref()
            .map(|s| render_text(s, values, true))
            .transpose()?;
        if subject.chars().any(char::is_control) {
            return Err(RenderError::InvalidPlaceholder);
        }
        Ok(RenderedTemplate {
            sender_identity_id: self.sender_identity_id.clone(),
            to: self.to.clone(),
            cc: self.cc.clone(),
            bcc: self.bcc.clone(),
            allow_caller_recipients: self.allow_caller_recipients,
            subject,
            text,
            html,
        })
    }
}

fn render_text(
    input: &str,
    values: &Map<String, Value>,
    html: bool,
) -> Result<String, RenderError> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or(RenderError::InvalidPlaceholder)?;
        let key = &after[..end];
        if !valid_identifier(key) {
            return Err(RenderError::InvalidPlaceholder);
        }
        let value = values
            .get(key)
            .ok_or_else(|| RenderError::MissingVariable(key.into()))?;
        let rendered = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => return Err(RenderError::InvalidVariable(key.into())),
        };
        if html {
            out.push_str(&escape_html(&rendered));
        } else {
            out.push_str(&rendered);
        }
        rest = &after[end + 2..];
    }
    if rest.contains("}}") {
        return Err(RenderError::InvalidPlaceholder);
    }
    out.push_str(rest);
    Ok(out)
}
fn valid_identifier(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some('a'..='z' | 'A'..='Z' | '_'))
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        out.push_str(match c {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' => "&quot;",
            '\'' => "&#39;",
            _ => {
                out.push(c);
                continue;
            }
        });
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateInput {
    pub scenario_id: String,
    pub name: String,
    pub enabled: Option<bool>,
    pub allow_caller_recipients: Option<bool>,
    pub sender_identity_id: String,
    pub subject_template: String,
    pub text_template: Option<String>,
    pub html_template: Option<String>,
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
}

#[derive(FromRow)]
struct SqliteRow {
    id: String,
    scenario_id: String,
    name: String,
    enabled: bool,
    allow_caller_recipients: bool,
    sender_identity_id: String,
    subject_template: String,
    text_template: Option<String>,
    html_template: Option<String>,
    to_json: String,
    cc_json: String,
    bcc_json: String,
    version: i64,
    created_at: String,
    updated_at: String,
}
#[derive(FromRow)]
struct MysqlRow {
    id: String,
    scenario_id: String,
    name: String,
    enabled: bool,
    allow_caller_recipients: bool,
    sender_identity_id: String,
    subject_template: String,
    text_template: Option<String>,
    html_template: Option<String>,
    to_json: sqlx::types::Json<Vec<String>>,
    cc_json: sqlx::types::Json<Vec<String>>,
    bcc_json: sqlx::types::Json<Vec<String>>,
    version: i64,
    created_at: NaiveDateTime,
    updated_at: NaiveDateTime,
}
impl TryFrom<SqliteRow> for Template {
    type Error = serde_json::Error;

    fn try_from(r: SqliteRow) -> Result<Self, Self::Error> {
        let variables = Template::variables(
            &r.subject_template,
            r.text_template.as_deref(),
            r.html_template.as_deref(),
        );
        Ok(Self {
            id: r.id,
            scenario_id: r.scenario_id,
            name: r.name,
            enabled: r.enabled,
            allow_caller_recipients: r.allow_caller_recipients,
            sender_identity_id: r.sender_identity_id,
            subject_template: r.subject_template,
            text_template: r.text_template,
            html_template: r.html_template,
            variables,
            to: serde_json::from_str(&r.to_json)?,
            cc: serde_json::from_str(&r.cc_json)?,
            bcc: serde_json::from_str(&r.bcc_json)?,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}
fn time(t: NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}
impl From<MysqlRow> for Template {
    fn from(r: MysqlRow) -> Self {
        let variables = Template::variables(
            &r.subject_template,
            r.text_template.as_deref(),
            r.html_template.as_deref(),
        );
        Self {
            id: r.id,
            scenario_id: r.scenario_id,
            name: r.name,
            enabled: r.enabled,
            allow_caller_recipients: r.allow_caller_recipients,
            sender_identity_id: r.sender_identity_id,
            subject_template: r.subject_template,
            text_template: r.text_template,
            html_template: r.html_template,
            variables,
            to: r.to_json.0,
            cc: r.cc_json.0,
            bcc: r.bcc_json.0,
            version: r.version,
            created_at: time(r.created_at),
            updated_at: time(r.updated_at),
        }
    }
}

pub async fn resolve(db: &Database, scenario_id: &str) -> Result<Option<Template>, sqlx::Error> {
    match db {
        Database::Sqlite(pool) => {
            let row = sqlx::query_as::<_, SqliteRow>(
                "SELECT * FROM mail_templates WHERE scenario_id=? AND enabled=1",
            )
            .bind(scenario_id)
            .fetch_optional(pool)
            .await?;
            row.map(TryInto::try_into)
                .transpose()
                .map_err(|error| sqlx::Error::Decode(Box::new(error)))
        }
        Database::Mysql(pool) => {
            let row = sqlx::query_as::<_, MysqlRow>(
                "SELECT id,scenario_id,name,enabled,allow_caller_recipients,sender_identity_id,subject_template,text_template,html_template,to_json,cc_json,bcc_json,version,created_at,updated_at FROM mail_templates WHERE scenario_id=? AND enabled=1",
            )
            .bind(scenario_id)
            .fetch_optional(pool)
            .await?;
            Ok(row.map(Into::into))
        }
    }
}

fn valid_address(v: &str) -> bool {
    if v.is_empty()
        || v.len() > 320
        || v.bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
    {
        return false;
    }
    let Some((l, d)) = v.split_once('@') else {
        return false;
    };
    !l.is_empty()
        && !d.is_empty()
        && !d.contains('@')
        && l.len() <= 64
        && d.len() <= 255
        && !l.starts_with('.')
        && !l.ends_with('.')
        && !l.contains("..")
        && l.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
        && d.split('.').all(|x| {
            !x.is_empty()
                && x.len() <= 63
                && x.as_bytes()[0].is_ascii_alphanumeric()
                && x.as_bytes()[x.len() - 1].is_ascii_alphanumeric()
                && x.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
fn validate(i: &TemplateInput) -> Result<(), &'static str> {
    let allow = i.allow_caller_recipients.unwrap_or(false);
    if i.scenario_id.trim().is_empty()
        || i.scenario_id.len() > 191
        || !i
            .scenario_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        || i.name.trim().is_empty()
        || i.name.len() > 191
        || i.name.chars().any(char::is_control)
        || i.sender_identity_id.is_empty()
        || i.subject_template.is_empty()
        || i.subject_template.len() > 998
        || i.subject_template.chars().any(char::is_control)
        || i.text_template
            .as_ref()
            .is_some_and(|s| s.len() > 12 * 1024 * 1024)
        || i.html_template
            .as_ref()
            .is_some_and(|s| s.len() > 12 * 1024 * 1024)
        || (!i.text_template.as_deref().is_some_and(|s| !s.is_empty())
            && !i.html_template.as_deref().is_some_and(|s| !s.is_empty()))
        || (!allow && i.to.is_empty())
        || i.to.len() + i.cc.len() + i.bcc.len() > 100
        || i.to
            .iter()
            .chain(&i.cc)
            .chain(&i.bcc)
            .any(|a| !valid_address(a))
    {
        return Err("invalid template settings");
    }
    for s in std::iter::once(i.subject_template.as_str())
        .chain(i.text_template.as_deref())
        .chain(i.html_template.as_deref())
    {
        validate_placeholders(s).map_err(|_| "invalid placeholder")?;
    }
    Ok(())
}
fn validate_placeholders(s: &str) -> Result<(), RenderError> {
    let mut r = s;
    while let Some(i) = r.find("{{") {
        let a = &r[i + 2..];
        let e = a.find("}}").ok_or(RenderError::InvalidPlaceholder)?;
        if !valid_identifier(&a[..e]) {
            return Err(RenderError::InvalidPlaceholder);
        }
        r = &a[e + 2..];
    }
    if r.contains("}}") {
        Err(RenderError::InvalidPlaceholder)
    } else {
        Ok(())
    }
}
async fn identity_ok(db: &Database, id: &str) -> Result<bool, sqlx::Error> {
    let q="SELECT EXISTS(SELECT 1 FROM sender_identities i JOIN smtp_accounts a ON a.id=i.smtp_account_id WHERE i.id=? AND i.enabled=1 AND i.deleted_at IS NULL AND a.enabled=1 AND a.deleted_at IS NULL)";
    match db {
        Database::Sqlite(p) => sqlx::query_scalar(q).bind(id).fetch_one(p).await,
        Database::Mysql(p) => sqlx::query_scalar(q).bind(id).fetch_one(p).await,
    }
}
fn result(status: u16, body: Option<Value>) -> ManagementResult {
    ManagementResult { status, body }
}
fn json_result<T: Serialize>(status: u16, v: T) -> ManagementResult {
    result(status, serde_json::to_value(v).ok())
}
fn api_error(status: u16, code: &str, message: &str) -> ManagementResult {
    result(
        status,
        Some(serde_json::json!({"code":code,"message":message})),
    )
}
fn diagnostic(e: impl std::fmt::Debug) -> PluginError {
    PluginError::new(format!("mail-template: {e:?}"))
}
async fn list_templates(db: &Database) -> Result<Vec<Template>, PluginError> {
    match db {
        Database::Sqlite(p)=>{ let rows=sqlx::query_as::<_,SqliteRow>("SELECT * FROM mail_templates ORDER BY scenario_id").fetch_all(p).await.map_err(diagnostic)?; rows.into_iter().map(TryInto::try_into).collect::<Result<Vec<_>,_>>().map_err(diagnostic) }
        Database::Mysql(p)=>sqlx::query_as::<_,MysqlRow>("SELECT id,scenario_id,name,enabled,allow_caller_recipients,sender_identity_id,subject_template,text_template,html_template,to_json,cc_json,bcc_json,version,created_at,updated_at FROM mail_templates ORDER BY scenario_id").fetch_all(p).await.map(|r|r.into_iter().map(Into::into).collect()).map_err(diagnostic)
    }
}
async fn get_template(db: &Database, id: &str) -> Result<Option<Template>, PluginError> {
    match db {
        Database::Sqlite(p)=>sqlx::query_as::<_,SqliteRow>("SELECT * FROM mail_templates WHERE id=?").bind(id).fetch_optional(p).await.map_err(diagnostic)?.map(TryInto::try_into).transpose().map_err(diagnostic),
        Database::Mysql(p)=>sqlx::query_as::<_,MysqlRow>("SELECT id,scenario_id,name,enabled,allow_caller_recipients,sender_identity_id,subject_template,text_template,html_template,to_json,cc_json,bcc_json,version,created_at,updated_at FROM mail_templates WHERE id=?").bind(id).fetch_optional(p).await.map(|r|r.map(Into::into)).map_err(diagnostic)
    }
}
async fn save_template(
    db: &Database,
    id: &str,
    i: &TemplateInput,
    create: bool,
) -> Result<ManagementResult, PluginError> {
    if let Err(msg) = validate(i) {
        return Ok(api_error(400, "invalid_request", msg));
    }
    if !identity_ok(db, &i.sender_identity_id)
        .await
        .map_err(diagnostic)?
    {
        return Ok(api_error(
            400,
            "invalid_request",
            "sender identity is unavailable",
        ));
    }
    let now = Utc::now();
    let duplicate_msg = "scenario_id is already in use";
    let outcome: Result<u64, sqlx::Error> = match db {
        Database::Sqlite(p) => {
            let mut q = if create {
                sqlx::query("INSERT INTO mail_templates(id,scenario_id,name,enabled,allow_caller_recipients,sender_identity_id,subject_template,text_template,html_template,to_json,cc_json,bcc_json,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)").bind(id)
            } else {
                sqlx::query("UPDATE mail_templates SET scenario_id=?,name=?,enabled=?,allow_caller_recipients=?,sender_identity_id=?,subject_template=?,text_template=?,html_template=?,to_json=?,cc_json=?,bcc_json=?,version=version+1,updated_at=? WHERE id=?")
            };
            q = q
                .bind(&i.scenario_id)
                .bind(&i.name)
                .bind(i.enabled.unwrap_or(true))
                .bind(i.allow_caller_recipients.unwrap_or(false))
                .bind(&i.sender_identity_id)
                .bind(&i.subject_template)
                .bind(&i.text_template)
                .bind(&i.html_template)
                .bind(serde_json::to_string(&i.to).unwrap_or_default())
                .bind(serde_json::to_string(&i.cc).unwrap_or_default())
                .bind(serde_json::to_string(&i.bcc).unwrap_or_default());
            q = if create {
                q.bind(now.to_rfc3339()).bind(now.to_rfc3339())
            } else {
                q.bind(now.to_rfc3339()).bind(id)
            };
            q.execute(p).await.map(|x| x.rows_affected())
        }
        Database::Mysql(p) => {
            let mut q = if create {
                sqlx::query("INSERT INTO mail_templates(id,scenario_id,name,enabled,allow_caller_recipients,sender_identity_id,subject_template,text_template,html_template,to_json,cc_json,bcc_json,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)").bind(id)
            } else {
                sqlx::query("UPDATE mail_templates SET scenario_id=?,name=?,enabled=?,allow_caller_recipients=?,sender_identity_id=?,subject_template=?,text_template=?,html_template=?,to_json=?,cc_json=?,bcc_json=?,version=version+1,updated_at=? WHERE id=?")
            };
            q = q
                .bind(&i.scenario_id)
                .bind(&i.name)
                .bind(i.enabled.unwrap_or(true))
                .bind(i.allow_caller_recipients.unwrap_or(false))
                .bind(&i.sender_identity_id)
                .bind(&i.subject_template)
                .bind(&i.text_template)
                .bind(&i.html_template)
                .bind(sqlx::types::Json(&i.to))
                .bind(sqlx::types::Json(&i.cc))
                .bind(sqlx::types::Json(&i.bcc));
            q = if create {
                q.bind(now.naive_utc()).bind(now.naive_utc())
            } else {
                q.bind(now.naive_utc()).bind(id)
            };
            q.execute(p).await.map(|x| x.rows_affected())
        }
    };
    match outcome {
        Ok(n) if n > 0 => {}
        Ok(_) => return Ok(api_error(404, "not_found", "template not found")),
        Err(e)
            if e.as_database_error()
                .is_some_and(|d| d.is_unique_violation()) =>
        {
            return Ok(api_error(409, "scenario_id_conflict", duplicate_msg))
        }
        Err(e) => return Err(diagnostic(e)),
    }
    let t = get_template(db, id)
        .await?
        .ok_or_else(|| diagnostic("saved template could not be loaded"))?;
    Ok(json_result(if create { 201 } else { 200 }, t))
}

pub struct TemplatePlugin {
    database: Database,
}
impl TemplatePlugin {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}
#[postwarden_plugin_api::async_trait]
impl Plugin for TemplatePlugin {
    fn id(&self) -> &'static str {
        "mail-template"
    }
    fn version(&self) -> &'static str {
        "1"
    }
    async fn compose(
        &self,
        draft: &MailDraft,
        input: &Value,
        _context: &PluginContext,
    ) -> Result<HookDecision, PluginError> {
        let Some(scenario) = input
            .get("scenario_id")
            .or_else(|| input.get("id"))
            .and_then(Value::as_str)
        else {
            return Ok(HookDecision::Reject {
                code: "template_scenario_required".into(),
            });
        };
        let vars = match input.get("variables") {
            None => Map::new(),
            Some(Value::Object(values)) => values.clone(),
            Some(_) => {
                return Ok(HookDecision::Reject {
                    code: "template_variables_invalid".into(),
                })
            }
        };
        let Some(t) = resolve(&self.database, scenario)
            .await
            .map_err(diagnostic)?
        else {
            return Ok(HookDecision::Reject {
                code: "template_not_found".into(),
            });
        };
        let rendered = match t.render(&vars) {
            Ok(rendered) => rendered,
            Err(_) => {
                return Ok(HookDecision::Reject {
                    code: "template_variables_invalid".into(),
                })
            }
        };
        let mut changed = draft.clone();
        changed.sender_identity_id = rendered.sender_identity_id;
        changed.subject = rendered.subject;
        changed.text_body = rendered.text;
        changed.html_body = rendered.html;
        let mut recipients = Vec::new();
        for a in rendered.to {
            recipients.push(Recipient::To(Mailbox {
                name: None,
                address: a,
            }));
        }
        for a in rendered.cc {
            recipients.push(Recipient::Cc(Mailbox {
                name: None,
                address: a,
            }));
        }
        for a in rendered.bcc {
            recipients.push(Recipient::Bcc(Mailbox {
                name: None,
                address: a,
            }));
        }
        if rendered.allow_caller_recipients {
            recipients.extend(draft.recipients.clone());
        }
        changed.recipients = recipients;
        Ok(HookDecision::Modify { draft: changed })
    }
    async fn manage(
        &self,
        resource: &str,
        operation: ManagementOperation,
        input: Option<Value>,
        _context: &PluginContext,
    ) -> Result<ManagementResult, PluginError> {
        if resource != "templates" {
            return Ok(result(404, None));
        }
        match operation {
            ManagementOperation::List => {
                Ok(json_result(200, list_templates(&self.database).await?))
            }
            ManagementOperation::Get { id } => match get_template(&self.database, &id).await? {
                Some(t) => Ok(json_result(200, t)),
                None => Ok(api_error(404, "not_found", "template not found")),
            },
            ManagementOperation::Create | ManagementOperation::Update { id: _ } => {
                let create = matches!(operation, ManagementOperation::Create);
                let id = match &operation {
                    ManagementOperation::Update { id } => id.clone(),
                    _ => Uuid::new_v4().to_string(),
                };
                let Some(v) = input else {
                    return Ok(api_error(
                        400,
                        "invalid_request",
                        "template body is required",
                    ));
                };
                let i: TemplateInput = match serde_json::from_value(v) {
                    Ok(i) => i,
                    Err(_) => {
                        return Ok(api_error(
                            400,
                            "invalid_request",
                            "template body is invalid",
                        ))
                    }
                };
                save_template(&self.database, &id, &i, create).await
            }
            ManagementOperation::Delete { id } => {
                let n = match &self.database {
                    Database::Sqlite(p) => sqlx::query("DELETE FROM mail_templates WHERE id=?")
                        .bind(&id)
                        .execute(p)
                        .await
                        .map_err(diagnostic)?
                        .rows_affected(),
                    Database::Mysql(p) => sqlx::query("DELETE FROM mail_templates WHERE id=?")
                        .bind(&id)
                        .execute(p)
                        .await
                        .map_err(diagnostic)?
                        .rows_affected(),
                };
                if n == 0 {
                    Ok(api_error(404, "not_found", "template not found"))
                } else {
                    Ok(result(204, None))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn render_escapes_html_and_rejects_missing_or_bad_variables() {
        let t = Template {
            id: "id".into(),
            scenario_id: "event".into(),
            name: "n".into(),
            enabled: true,
            allow_caller_recipients: false,
            sender_identity_id: "s".into(),
            subject_template: "Hello {{name}}".into(),
            text_template: None,
            html_template: Some("<b>{{name}}</b>".into()),
            variables: vec!["name".into()],
            to: vec!["x@y.com".into()],
            cc: vec![],
            bcc: vec![],
            version: 1,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let mut v = Map::new();
        v.insert("name".into(), Value::String("<x&>".into()));
        let out = t.render(&v).unwrap();
        assert_eq!(out.html.as_deref(), Some("<b>&lt;x&amp;&gt;</b>"));
        v.clear();
        assert_eq!(
            t.render(&v),
            Err(RenderError::MissingVariable("name".into()))
        );
        let mut broken = t;
        broken.subject_template = "{{bad-key}}".into();
        assert_eq!(
            broken.render(&Map::new()),
            Err(RenderError::InvalidPlaceholder)
        );
    }

    #[test]
    fn extracts_deduplicated_variables_in_template_order() {
        assert_eq!(
            Template::variables(
                "Hello {{name}}, {{first_name}}",
                Some("{{name}} {{order_id}}"),
                Some("{{order_id}} {{_footer}}"),
            ),
            vec!["name", "first_name", "order_id", "_footer"]
        );
    }
}
