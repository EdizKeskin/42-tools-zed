use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
};

use chrono::Local;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    process::Command as TokioCommand,
    sync::RwLock,
};
use tower_lsp::{
    jsonrpc::Result,
    lsp_types::{
        CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
        CodeActionProviderCapability, CodeActionResponse, DidChangeConfigurationParams,
        DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
        DidSaveTextDocumentParams, DocumentFormattingParams, ExecuteCommandOptions,
        ExecuteCommandParams, InitializeParams, InitializeResult, MessageType, OneOf, Position,
        Range, ServerCapabilities, ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind,
        TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit, Url,
        WillSaveTextDocumentParams, WorkspaceEdit,
    },
    Client, LanguageServer, LspService, Server,
};

const INSERT_HEADER_COMMAND: &str = "42tools.insertHeader";
const HEADER_CODE_ACTION_KIND: &str = "source.42tools.header";
const HEADER_ACTION_TITLE: &str = "42 Tools: Insert or Update Header";
const FORMATTER_BINARY: &str = "c_formatter_42";
const PYTHON_BINARY: &str = "python3";
const PYTHON_FORMATTER_MODULE: &str = "c_formatter_42";
const DEFAULT_EMAIL_DOMAIN: &str = "student.42istanbul.com.tr";
const TIMESTAMP_FORMAT: &str = "%Y/%m/%d %H:%M:%S";
const SETTINGS_ENV_VAR: &str = "FORTY_TWO_TOOLS_SETTINGS_JSON";

const HEADER_LINE_COUNT: usize = 11;
const HEADER_UPDATED_LINE_INDEX: usize = 8;
const FILE_FIELD_WIDTH: usize = 51;
const AUTHOR_FIELD_WIDTH: usize = 43;
const CREATED_LOGIN_WIDTH: usize = 18;
const UPDATED_LOGIN_WIDTH: usize = 17;

const HEADER_TOP_BORDER: &str =
    "/* ************************************************************************** */";
const HEADER_EMPTY_LINE: &str =
    "/*                                                                            */";
const HEADER_FILE_HINT_LINE: &str =
    "/*                                                        :::      ::::::::   */";
const HEADER_KAA_LINE: &str =
    "/*                                                    +:+ +:+         +:+     */";
const HEADER_HASH_LINE: &str =
    "/*                                                +#+#+#+#+#+   +#+           */";

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthorIdentity {
    login: String,
    email: String,
}

impl AuthorIdentity {
    fn from_settings(settings: &HeaderSettings) -> Self {
        let login = settings
            .login
            .as_deref()
            .and_then(trimmed_option)
            .map(ToString::to_string)
            .unwrap_or_else(resolve_login);
        let email_domain = settings
            .email_domain
            .as_deref()
            .and_then(trimmed_option)
            .unwrap_or(DEFAULT_EMAIL_DOMAIN);
        let email = format!("{login}@{email_domain}");
        Self { login, email }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct RuntimeSettings {
    formatter: FormatterSettings,
    header: HeaderSettings,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct FormatterSettings {
    path: Option<String>,
    arguments: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct HeaderSettings {
    login: Option<String>,
    email_domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormatterCommand {
    program: String,
    arguments: Vec<String>,
}

#[derive(Debug)]
enum FormatterError {
    Spawn {
        program: String,
        error: String,
    },
    Stdin {
        program: String,
        error: String,
    },
    Exit {
        program: String,
        status: String,
        stderr: String,
    },
    InvalidUtf8 {
        program: String,
        error: String,
    },
}

impl FormatterError {
    fn message(&self) -> String {
        match self {
            Self::Spawn { program, error } => format!("failed to launch `{program}`: {error}"),
            Self::Stdin { program, error } => {
                format!("failed to stream text to `{program}`: {error}")
            }
            Self::Exit {
                program,
                status,
                stderr,
            } => {
                if stderr.is_empty() {
                    format!("`{program}` exited unsuccessfully: {status}")
                } else {
                    format!("`{program}` exited unsuccessfully: {status}; stderr: {stderr}")
                }
            }
            Self::InvalidUtf8 { program, error } => {
                format!("`{program}` produced invalid UTF-8 output: {error}")
            }
        }
    }
}

impl FormatterCommand {
    fn python_module_formatter() -> Self {
        Self {
            program: PYTHON_BINARY.to_string(),
            arguments: vec!["-m".to_string(), PYTHON_FORMATTER_MODULE.to_string()],
        }
    }
}

struct Backend {
    client: Client,
    documents: RwLock<HashMap<Url, String>>,
    runtime_settings: RwLock<RuntimeSettings>,
}

impl Backend {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: RwLock::new(HashMap::new()),
            runtime_settings: RwLock::new(load_initial_runtime_settings()),
        }
    }

    async fn log_warning(&self, message: impl Into<String>) {
        self.client
            .log_message(MessageType::WARNING, message.into())
            .await;
    }

    async fn log_info(&self, message: impl Into<String>) {
        self.client
            .log_message(MessageType::INFO, message.into())
            .await;
    }

    async fn read_document_text(&self, uri: &Url) -> std::result::Result<String, String> {
        if let Some(text) = self.documents.read().await.get(uri).cloned() {
            return Ok(text);
        }

        let path = uri_to_path(uri)?;
        tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| format!("failed to read `{}` from disk: {error}", path.display()))
    }

    async fn read_runtime_settings(&self) -> RuntimeSettings {
        self.runtime_settings.read().await.clone()
    }

    async fn apply_runtime_settings(&self, settings: Value) -> std::result::Result<(), String> {
        let parsed = parse_runtime_settings(&settings)?;
        *self.runtime_settings.write().await = parsed;
        Ok(())
    }

    async fn header_workspace_edit(&self, uri: &Url) -> std::result::Result<WorkspaceEdit, String> {
        let source = self.read_document_text(uri).await?;
        let runtime_settings = self.read_runtime_settings().await;
        build_header_workspace_edit(uri, &source, &runtime_settings.header)
    }

    async fn save_pipeline_edits(&self, uri: &Url) -> Result<Option<Vec<TextEdit>>> {
        if !is_supported_c_document(uri) {
            return Ok(None);
        }

        let source = match self.read_document_text(uri).await {
            Ok(source) => source,
            Err(error) => {
                self.log_warning(error).await;
                return Ok(None);
            }
        };

        let runtime_settings = self.read_runtime_settings().await;
        let formatted = match run_formatter(&runtime_settings.formatter, &source).await {
            Ok(formatted) => formatted,
            Err(error) => {
                self.log_warning(error.message()).await;
                source.clone()
            }
        };

        let Some(file_name) = file_name_from_uri(&uri) else {
            self.log_warning(format!("could not resolve a file name from `{uri}`"))
                .await;
            if formatted == source {
                return Ok(None);
            }
            return Ok(Some(vec![full_document_edit(&source, formatted)]));
        };

        let identity = AuthorIdentity::from_settings(&runtime_settings.header);
        let timestamp = current_timestamp();
        let output =
            match render_document_with_header(&formatted, &file_name, &identity, &timestamp) {
                Ok(output) => output,
                Err(error) => {
                    self.log_warning(error).await;
                    formatted
                }
            };

        if output == source {
            Ok(None)
        } else {
            Ok(Some(vec![full_document_edit(&source, output)]))
        }
    }

    async fn format_document_edits(
        &self,
        params: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        self.save_pipeline_edits(&params.text_document.uri).await
    }

    async fn header_code_actions(
        &self,
        params: CodeActionParams,
    ) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        if !is_supported_c_document(&uri) {
            return Ok(None);
        }

        let source = match self.read_document_text(&uri).await {
            Ok(source) => source,
            Err(error) => {
                self.log_warning(error).await;
                return Ok(None);
            }
        };

        let runtime_settings = self.read_runtime_settings().await;
        let edit = match build_header_workspace_edit(&uri, &source, &runtime_settings.header) {
            Ok(edit) => edit,
            Err(error) => {
                self.log_warning(error).await;
                return Ok(None);
            }
        };

        let action = CodeAction {
            title: HEADER_ACTION_TITLE.to_string(),
            kind: Some(CodeActionKind::from(HEADER_CODE_ACTION_KIND.to_string())),
            edit: Some(edit),
            is_preferred: Some(true),
            ..Default::default()
        };

        Ok(Some(vec![CodeActionOrCommand::CodeAction(action)]))
    }

    async fn execute_header_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        if params.command != INSERT_HEADER_COMMAND {
            self.log_warning(format!("unknown command requested: {}", params.command))
                .await;
            return Ok(None);
        }

        let Some(uri) = params.arguments.first().and_then(parse_command_uri) else {
            self.log_warning("`42tools.insertHeader` was called without a valid document URI")
                .await;
            return Ok(None);
        };

        let edit = match self.header_workspace_edit(&uri).await {
            Ok(edit) => edit,
            Err(error) => {
                self.log_warning(error).await;
                return Ok(None);
            }
        };

        match self.client.apply_edit(edit).await {
            Ok(response) if response.applied => {}
            Ok(response) => {
                let reason = response
                    .failure_reason
                    .unwrap_or_else(|| "the client rejected the edit".to_string());
                self.log_warning(format!("failed to apply 42 header edit: {reason}"))
                    .await;
            }
            Err(error) => {
                self.log_warning(format!(
                    "failed to send 42 header edit to the client: {error}"
                ))
                .await;
            }
        }

        Ok(None)
    }

    async fn apply_workspace_edit(&self, uri: &Url, edits: Vec<TextEdit>, context: &str) {
        let workspace_edit = WorkspaceEdit {
            changes: Some(HashMap::from([(uri.clone(), edits)])),
            ..Default::default()
        };

        match self.client.apply_edit(workspace_edit).await {
            Ok(response) if response.applied => {}
            Ok(response) => {
                let reason = response
                    .failure_reason
                    .unwrap_or_else(|| "the client rejected the edit".to_string());
                self.log_warning(format!("failed to apply {context}: {reason}"))
                    .await;
            }
            Err(error) => {
                self.log_warning(format!("failed to send {context} to the client: {error}"))
                    .await;
            }
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "42 Tools LSP".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        will_save: Some(true),
                        will_save_wait_until: Some(true),
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                    },
                )),
                document_formatting_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(
                    tower_lsp::lsp_types::CodeActionOptions {
                        code_action_kinds: Some(vec![CodeActionKind::from(
                            HEADER_CODE_ACTION_KIND.to_string(),
                        )]),
                        resolve_provider: Some(false),
                        ..Default::default()
                    },
                )),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![INSERT_HEADER_COMMAND.to_string()],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: tower_lsp::lsp_types::InitializedParams) {
        self.log_info("42 Tools LSP initialized").await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.documents
            .write()
            .await
            .insert(params.text_document.uri, params.text_document.text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents
                .write()
                .await
                .insert(params.text_document.uri, change.text);
        }
    }

    async fn will_save_wait_until(
        &self,
        params: WillSaveTextDocumentParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        self.save_pipeline_edits(&params.text_document.uri).await
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        match self.save_pipeline_edits(&params.text_document.uri).await {
            Ok(Some(edits)) => {
                self.apply_workspace_edit(
                    &params.text_document.uri,
                    edits,
                    "42 Tools save pipeline edits",
                )
                .await;
            }
            Ok(None) => {}
            Err(error) => self.log_warning(error.to_string()).await,
        }
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        match self.apply_runtime_settings(params.settings).await {
            Ok(()) => self.log_info("42 Tools settings updated").await,
            Err(error) => {
                self.log_warning(format!("failed to apply runtime settings: {error}"))
                    .await
            }
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        self.format_document_edits(params).await
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        self.header_code_actions(params).await
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<Value>> {
        self.execute_header_command(params).await
    }
}

fn load_initial_runtime_settings() -> RuntimeSettings {
    let Some(raw_settings) = env::var(SETTINGS_ENV_VAR)
        .ok()
        .and_then(|value| trimmed_option(&value).map(ToString::to_string))
    else {
        return RuntimeSettings::default();
    };

    serde_json::from_str(&raw_settings).unwrap_or_default()
}

fn parse_runtime_settings(value: &Value) -> std::result::Result<RuntimeSettings, String> {
    if value.is_null() {
        return Ok(RuntimeSettings::default());
    }

    serde_json::from_value(value.clone()).map_err(|error| error.to_string())
}

fn resolve_login() -> String {
    let user = env::var("USER").ok();
    let username = env::var("USERNAME").ok();
    resolve_login_from_sources(user.as_deref(), username.as_deref())
}

fn resolve_login_from_sources(user: Option<&str>, username: Option<&str>) -> String {
    user.filter(|value| !value.trim().is_empty())
        .or_else(|| username.filter(|value| !value.trim().is_empty()))
        .unwrap_or("unknown")
        .trim()
        .to_string()
}

fn resolve_formatter_command(settings: &FormatterSettings) -> FormatterCommand {
    let program = settings
        .path
        .as_deref()
        .and_then(trimmed_option)
        .map(ToString::to_string)
        .unwrap_or_else(|| FORMATTER_BINARY.to_string());

    FormatterCommand {
        program,
        arguments: settings.arguments.clone(),
    }
}

async fn run_formatter(
    settings: &FormatterSettings,
    source: &str,
) -> std::result::Result<String, FormatterError> {
    if settings.path.as_deref().and_then(trimmed_option).is_some() {
        let formatter_command = resolve_formatter_command(settings);
        return run_formatter_command(
            &formatter_command.program,
            &formatter_command.arguments,
            source,
        )
        .await;
    }

    let candidates = [
        resolve_formatter_command(settings),
        FormatterCommand::python_module_formatter(),
    ];

    let mut last_spawn_error = None;
    for candidate in candidates {
        match run_formatter_command(&candidate.program, &candidate.arguments, source).await {
            Ok(output) => return Ok(output),
            Err(error @ FormatterError::Spawn { .. }) => {
                last_spawn_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_spawn_error.unwrap_or_else(|| FormatterError::Spawn {
        program: FORMATTER_BINARY.to_string(),
        error: "formatter executable could not be resolved".to_string(),
    }))
}

fn trimmed_option(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn parse_command_uri(value: &Value) -> Option<Url> {
    match value {
        Value::String(uri) => Url::parse(uri).ok(),
        Value::Object(object) => object
            .get("uri")
            .and_then(Value::as_str)
            .and_then(|uri| Url::parse(uri).ok()),
        _ => None,
    }
}

fn uri_to_path(uri: &Url) -> std::result::Result<PathBuf, String> {
    uri.to_file_path()
        .map_err(|_| format!("unsupported non-file URI: {uri}"))
}

fn file_name_from_uri(uri: &Url) -> Option<String> {
    if let Ok(path) = uri_to_path(uri) {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            return Some(name.to_string());
        }
    }

    uri.path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
        .map(ToString::to_string)
}

fn is_supported_c_document(uri: &Url) -> bool {
    let Some(file_name) = file_name_from_uri(uri) else {
        return false;
    };

    let Some(extension) = Path::new(&file_name)
        .extension()
        .and_then(|extension| extension.to_str())
    else {
        return false;
    };

    matches!(extension.to_ascii_lowercase().as_str(), "c" | "h")
}

fn line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn trim_or_pad(input: &str, width: usize) -> String {
    let trimmed = input.chars().take(width).collect::<String>();
    format!("{trimmed:<width$}")
}

fn current_timestamp() -> String {
    Local::now().format(TIMESTAMP_FORMAT).to_string()
}

fn build_header_workspace_edit(
    uri: &Url,
    source: &str,
    header_settings: &HeaderSettings,
) -> std::result::Result<WorkspaceEdit, String> {
    let edit = build_header_text_edit(uri, source, header_settings)?;
    Ok(WorkspaceEdit {
        changes: Some(HashMap::from([(uri.clone(), vec![edit])])),
        ..Default::default()
    })
}

fn build_header_text_edit(
    uri: &Url,
    source: &str,
    header_settings: &HeaderSettings,
) -> std::result::Result<TextEdit, String> {
    let file_name = file_name_from_uri(uri)
        .ok_or_else(|| format!("could not resolve a file name from `{uri}`"))?;
    let identity = AuthorIdentity::from_settings(header_settings);
    let timestamp = current_timestamp();
    build_header_text_edit_with_identity(source, &file_name, &identity, &timestamp)
}

fn build_header_text_edit_with_identity(
    source: &str,
    file_name: &str,
    identity: &AuthorIdentity,
    timestamp: &str,
) -> std::result::Result<TextEdit, String> {
    if has_42_header(source) {
        let updated_line = build_updated_line(identity, timestamp);
        let current_line = document_line(source, HEADER_UPDATED_LINE_INDEX).ok_or_else(|| {
            "document ended unexpectedly while updating an existing header".to_string()
        })?;
        return Ok(TextEdit {
            range: Range::new(
                Position::new(HEADER_UPDATED_LINE_INDEX as u32, 0),
                Position::new(HEADER_UPDATED_LINE_INDEX as u32, utf16_len(current_line)),
            ),
            new_text: updated_line,
        });
    }

    let header = build_header_block(file_name, identity, timestamp, line_ending(source));
    Ok(TextEdit {
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        new_text: header,
    })
}

fn render_document_with_header(
    source: &str,
    file_name: &str,
    identity: &AuthorIdentity,
    timestamp: &str,
) -> std::result::Result<String, String> {
    if has_42_header(source) {
        let newline = line_ending(source);
        let has_trailing_newline = source.ends_with('\n');
        let normalized = source.replace("\r\n", "\n");
        let mut lines = normalized
            .lines()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        if lines.len() <= HEADER_UPDATED_LINE_INDEX {
            return Err(
                "document ended unexpectedly while updating an existing header".to_string(),
            );
        }

        lines[HEADER_UPDATED_LINE_INDEX] = build_updated_line(identity, timestamp);
        let mut output = lines.join(newline);
        if has_trailing_newline {
            output.push_str(newline);
        }
        return Ok(output);
    }

    Ok(format!(
        "{}{}",
        build_header_block(file_name, identity, timestamp, line_ending(source)),
        source
    ))
}

fn build_header_block(
    file_name: &str,
    identity: &AuthorIdentity,
    timestamp: &str,
    newline: &str,
) -> String {
    let author = trim_or_pad(
        &format!("{} <{}>", identity.login, identity.email),
        AUTHOR_FIELD_WIDTH,
    );
    let created_login = trim_or_pad(&identity.login, CREATED_LOGIN_WIDTH);
    let updated_login = trim_or_pad(&identity.login, UPDATED_LOGIN_WIDTH);
    let file_name = trim_or_pad(file_name, FILE_FIELD_WIDTH);

    [
        HEADER_TOP_BORDER.to_string(),
        HEADER_EMPTY_LINE.to_string(),
        HEADER_FILE_HINT_LINE.to_string(),
        format!("/*   {file_name}:+:      :+:    :+:   */"),
        HEADER_KAA_LINE.to_string(),
        format!("/*   By: {author}#+#  +:+       +#+        */"),
        HEADER_HASH_LINE.to_string(),
        format!("/*   Created: {timestamp} by {created_login}#+#    #+#             */"),
        format!("/*   Updated: {timestamp} by {updated_login}###   ########.fr       */"),
        HEADER_EMPTY_LINE.to_string(),
        HEADER_TOP_BORDER.to_string(),
    ]
    .join(newline)
        + newline
}

fn build_updated_line(identity: &AuthorIdentity, timestamp: &str) -> String {
    let updated_login = trim_or_pad(&identity.login, UPDATED_LOGIN_WIDTH);
    format!("/*   Updated: {timestamp} by {updated_login}###   ########.fr       */")
}

fn has_42_header(source: &str) -> bool {
    let normalized = source.replace("\r\n", "\n");
    let lines = normalized
        .lines()
        .take(HEADER_LINE_COUNT)
        .collect::<Vec<_>>();
    if lines.len() < HEADER_LINE_COUNT {
        return false;
    }

    lines[0] == HEADER_TOP_BORDER
        && lines[1] == HEADER_EMPTY_LINE
        && lines[2] == HEADER_FILE_HINT_LINE
        && lines[4] == HEADER_KAA_LINE
        && lines[6] == HEADER_HASH_LINE
        && lines[9] == HEADER_EMPTY_LINE
        && lines[10] == HEADER_TOP_BORDER
        && header_file_line_matches(lines[3])
        && header_author_line_matches(lines[5])
        && header_created_line_regex().is_match(lines[7])
        && header_updated_line_regex().is_match(lines[8])
        && lines.iter().all(|line| line.chars().count() == 80)
}

fn header_file_line_matches(line: &str) -> bool {
    line.starts_with("/*   ") && line.ends_with(":+:      :+:    :+:   */")
}

fn header_author_line_matches(line: &str) -> bool {
    line.starts_with("/*   By: ") && line.ends_with("#+#  +:+       +#+        */")
}

fn header_created_line_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"^/\*   Created: \d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} by .{18}#\+#\s{4}#\+#\s{13}\*/$",
        )
        .expect("valid created header regex")
    })
}

fn header_updated_line_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"^/\*   Updated: \d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} by .{17}###\s{3}########\.fr\s{7}\*/$",
        )
        .expect("valid updated header regex")
    })
}

fn document_line(text: &str, line_index: usize) -> Option<&str> {
    text.lines().nth(line_index)
}

fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

fn document_end_position(text: &str) -> Position {
    let mut line = 0_u32;
    let mut character = 0_u32;

    for ch in text.chars() {
        if ch == '\n' {
            line += 1;
            character = 0;
        } else if ch != '\r' {
            character += ch.len_utf16() as u32;
        }
    }

    Position::new(line, character)
}

fn full_document_edit(source: &str, replacement: String) -> TextEdit {
    TextEdit {
        range: Range::new(Position::new(0, 0), document_end_position(source)),
        new_text: replacement,
    }
}

async fn run_formatter_command(
    program: &str,
    args: &[String],
    source: &str,
) -> std::result::Result<String, FormatterError> {
    let mut command = TokioCommand::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| FormatterError::Spawn {
        program: program.to_string(),
        error: error.to_string(),
    })?;

    if let Some(stdin) = child.stdin.take() {
        let mut stdin = BufWriter::new(stdin);
        stdin
            .write_all(source.as_bytes())
            .await
            .map_err(|error| FormatterError::Stdin {
                program: program.to_string(),
                error: error.to_string(),
            })?;
        stdin.flush().await.map_err(|error| FormatterError::Stdin {
            program: program.to_string(),
            error: error.to_string(),
        })?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|error| FormatterError::Spawn {
            program: program.to_string(),
            error: error.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(FormatterError::Exit {
            program: program.to_string(),
            status: output.status.to_string(),
            stderr,
        });
    }

    String::from_utf8(output.stdout).map_err(|error| FormatterError::InvalidUtf8 {
        program: program.to_string(),
        error: error.to_string(),
    })
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn fixture_identity() -> AuthorIdentity {
        AuthorIdentity {
            login: "marvin".to_string(),
            email: "marvin@student.42istanbul.com.tr".to_string(),
        }
    }

    fn fixture_timestamp() -> &'static str {
        "2026/03/11 12:34:56"
    }

    fn fixture_uri() -> Url {
        Url::parse("file:///tmp/main.c").expect("valid URI")
    }

    fn create_script(name: &str, contents: &str) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temporary directory");
        let file_name = if cfg!(windows) {
            format!("{name}.cmd")
        } else {
            name.to_string()
        };
        let path = dir.path().join(file_name);
        fs::write(&path, contents).expect("script written");

        #[cfg(unix)]
        {
            let mut permissions = fs::metadata(&path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("permissions set");
        }

        (dir, path)
    }

    fn success_formatter_script() -> &'static str {
        if cfg!(windows) {
            "@echo off\r\npowershell -NoProfile -Command \"$inputData = [Console]::In.ReadToEnd(); [Console]::Out.Write($inputData.Replace('int', 'formatted int'))\"\r\n"
        } else {
            "#!/bin/sh\nsed 's/int/formatted int/'\n"
        }
    }

    fn failing_formatter_script() -> &'static str {
        if cfg!(windows) {
            "@echo off\r\nexit /b 7\r\n"
        } else {
            "#!/bin/sh\nexit 7\n"
        }
    }

    fn invalid_utf8_formatter_script() -> &'static str {
        if cfg!(windows) {
            "@echo off\r\npowershell -NoProfile -Command \"$stdout = [Console]::OpenStandardOutput(); $stdout.Write([byte[]](255), 0, 1)\"\r\n"
        } else {
            "#!/bin/sh\nprintf '\\377'\n"
        }
    }

    #[test]
    fn inserts_header_into_an_empty_document() {
        let edit = build_header_text_edit_with_identity(
            "",
            "main.c",
            &fixture_identity(),
            fixture_timestamp(),
        )
        .expect("header edit");

        assert_eq!(edit.range.start, Position::new(0, 0));
        assert_eq!(edit.range.end, Position::new(0, 0));
        assert!(edit.new_text.ends_with('\n'));
        assert!(has_42_header(&edit.new_text));
    }

    #[test]
    fn updates_only_the_updated_line_when_header_exists() {
        let header = build_header_block("main.c", &fixture_identity(), fixture_timestamp(), "\n");
        let source = format!("{header}int main(void)\n{{\n\treturn (0);\n}}\n");
        let new_timestamp = "2026/03/11 13:00:00";
        let edit = build_header_text_edit_with_identity(
            &source,
            "main.c",
            &fixture_identity(),
            new_timestamp,
        )
        .expect("header edit");

        assert_eq!(
            edit.range.start,
            Position::new(HEADER_UPDATED_LINE_INDEX as u32, 0)
        );
        assert_eq!(edit.range.end.line, HEADER_UPDATED_LINE_INDEX as u32);
        assert_eq!(
            edit.new_text,
            "/*   Updated: 2026/03/11 13:00:00 by marvin           ###   ########.fr       */"
        );
        assert!(source.contains("Created: 2026/03/11 12:34:56 by marvin"));
    }

    #[test]
    fn generated_header_contains_the_file_name() {
        let header = build_header_block("libft.h", &fixture_identity(), fixture_timestamp(), "\n");
        assert!(header.contains("libft.h"));
        assert!(header.lines().all(|line| line.chars().count() == 80));
    }

    #[test]
    fn detects_generated_headers() {
        let header = build_header_block("main.c", &fixture_identity(), fixture_timestamp(), "\n");
        assert!(has_42_header(&header));
    }

    #[test]
    fn rejects_non_header_documents() {
        let source = "/* not a 42 header */\nint main(void)\n{\n\treturn (0);\n}\n";
        assert!(!has_42_header(source));
    }

    #[test]
    fn resolve_login_uses_expected_fallback_order() {
        assert_eq!(
            resolve_login_from_sources(Some("marvin"), Some("fallback")),
            "marvin"
        );
        assert_eq!(
            resolve_login_from_sources(Some("   "), Some("marvin")),
            "marvin"
        );
        assert_eq!(resolve_login_from_sources(None, Some("marvin")), "marvin");
        assert_eq!(resolve_login_from_sources(None, None), "unknown");
    }

    #[test]
    fn header_settings_override_environment_defaults() {
        let identity = AuthorIdentity::from_settings(&HeaderSettings {
            login: Some("cadet".to_string()),
            email_domain: Some("student.42lyon.fr".to_string()),
        });

        assert_eq!(identity.login, "cadet");
        assert_eq!(identity.email, "cadet@student.42lyon.fr");
    }

    #[test]
    fn formatter_settings_path_has_highest_priority() {
        let command = resolve_formatter_command(&FormatterSettings {
            path: Some("/tmp/custom-formatter".to_string()),
            arguments: vec!["--flag".to_string()],
        });

        assert_eq!(command.program, "/tmp/custom-formatter");
        assert_eq!(command.arguments, vec!["--flag".to_string()]);
    }

    #[test]
    fn runtime_settings_are_loaded_from_json_values() {
        let settings = parse_runtime_settings(&serde_json::json!({
            "formatter": {
                "path": "/tmp/c_formatter_42",
                "arguments": ["--strict"]
            },
            "header": {
                "login": "marvin",
                "email_domain": "student.42istanbul.com.tr"
            }
        }))
        .expect("settings parsed");

        assert_eq!(
            settings.formatter.path.as_deref(),
            Some("/tmp/c_formatter_42")
        );
        assert_eq!(settings.formatter.arguments, vec!["--strict"]);
        assert_eq!(settings.header.login.as_deref(), Some("marvin"));
    }

    #[test]
    fn timestamp_format_matches_42_style() {
        let regex =
            Regex::new(r"^\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}$").expect("valid timestamp regex");
        assert!(regex.is_match(fixture_timestamp()));
    }

    #[test]
    fn workspace_edit_targets_the_requested_uri() {
        let uri = fixture_uri();
        let edit = build_header_workspace_edit(&uri, "", &HeaderSettings::default())
            .expect("workspace edit")
            .changes
            .expect("workspace changes");
        assert!(edit.contains_key(&uri));
    }

    #[tokio::test]
    async fn formatter_command_supports_successful_runs() {
        let (_dir, script) = create_script("formatter_success", success_formatter_script());
        let formatted = run_formatter_command(
            script.to_str().expect("script path"),
            &[],
            "int main(void)\n{\n\treturn (0);\n}\n",
        )
        .await
        .expect("formatted output");

        assert!(formatted.starts_with("formatted int"));
    }

    #[tokio::test]
    async fn formatter_command_reports_missing_binaries() {
        let result = run_formatter_command(
            "__missing_42_formatter_binary__",
            &[],
            "int main(void)\n{\n\treturn (0);\n}\n",
        )
        .await;

        assert!(matches!(result, Err(FormatterError::Spawn { .. })));
    }

    #[tokio::test]
    async fn formatter_command_reports_non_zero_exit_codes() {
        let (_dir, script) = create_script("formatter_failure", failing_formatter_script());
        let result = run_formatter_command(
            script.to_str().expect("script path"),
            &[],
            "int main(void)\n{\n\treturn (0);\n}\n",
        )
        .await;

        match result {
            Err(FormatterError::Exit { .. })
            | Err(FormatterError::Spawn { .. })
            | Err(FormatterError::Stdin { .. }) => {}
            Ok(output) => panic!("expected formatter failure, got successful output: {output}"),
            Err(other) => panic!("expected a formatter failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn formatter_command_reports_invalid_utf8() {
        let (_dir, script) =
            create_script("formatter_invalid_utf8", invalid_utf8_formatter_script());
        let result = run_formatter_command(
            script.to_str().expect("script path"),
            &[],
            "int main(void)\n{\n\treturn (0);\n}\n",
        )
        .await;

        assert!(matches!(result, Err(FormatterError::InvalidUtf8 { .. })));
    }
}
