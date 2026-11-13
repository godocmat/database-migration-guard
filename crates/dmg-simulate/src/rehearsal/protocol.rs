use std::{
    io,
    time::{Duration, Instant},
};

use bytes::BytesMut;
use fallible_iterator::FallibleIterator;
use postgres_protocol::{
    IsNull,
    authentication::sasl::{ChannelBinding, SCRAM_SHA_256, ScramSha256},
    message::{
        backend::{ErrorResponseBody, Header, Message},
        frontend,
    },
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use crate::{DockerWorker, WorkerStream};

use super::plan::PlannedQuery;

const MAX_FRAME: usize = 1024 * 1024;
const MAX_RESPONSE: usize = 16 * 1024 * 1024;
const MAX_RESPONSE_MESSAGES: usize = 65_536;
const MAX_REQUEST: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Ready {
    Idle,
    Transaction,
    FailedTransaction,
}

pub(super) struct Session<S> {
    stream: S,
}

#[derive(Debug)]
pub(super) enum ProtocolError {
    Transport,
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    Deadline,
    Invalid,
    Limit,
    Database(String),
}

pub(super) struct QueryResult {
    pub(super) ready: Ready,
    pub(super) completed: Vec<CommandCompletion>,
    pub(super) sqlstate: Option<String>,
}

pub(super) struct CommandCompletion {
    pub(super) kind: CommandKind,
    pub(super) elapsed: Duration,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum CommandKind {
    Commit,
    Rollback,
    Insert(u64),
    Other,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Session<WorkerStream<'_>> {
    pub(super) async fn connect<'worker>(
        worker: &'worker DockerWorker,
    ) -> Result<Session<WorkerStream<'worker>>, ProtocolError> {
        let stream = worker.connect_stream().map_err(|error| match error {
            crate::WorkerError::Deadline(_) => ProtocolError::Deadline,
            _ => ProtocolError::Transport,
        })?;
        Session::startup(stream, &worker.connection_config()).await
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    pub(super) async fn startup(
        stream: S,
        config: &tokio_postgres::Config,
    ) -> Result<Self, ProtocolError> {
        let mut session = Self { stream };
        let user = config.get_user().ok_or(ProtocolError::Invalid)?;
        let mut startup = BytesMut::new();
        let mut parameters = vec![("user", user), ("client_encoding", "UTF8")];
        if let Some(database) = config.get_dbname() {
            parameters.push(("database", database));
        }
        if let Some(options) = config.get_options() {
            parameters.push(("options", options));
        }
        if let Some(application) = config.get_application_name() {
            parameters.push(("application_name", application));
        }
        frontend::startup_message(parameters, &mut startup).map_err(invalid)?;
        session.write(&startup).await?;
        let password = config.get_password().ok_or(ProtocolError::Invalid)?;
        session.authenticate(password).await?;
        session.startup_ready().await?;
        Ok(session)
    }
    async fn authenticate(&mut self, password: &[u8]) -> Result<(), ProtocolError> {
        let mut budget = ResponseBudget::default();
        let Message::AuthenticationSasl(body) = self.authentication_message(&mut budget).await?
        else {
            return Err(ProtocolError::Invalid);
        };
        let mut mechanisms = body.mechanisms();
        if !mechanisms
            .any(|name| Ok(name == SCRAM_SHA_256))
            .map_err(invalid)?
        {
            return Err(ProtocolError::Invalid);
        }
        let mut scram = ScramSha256::new(password, ChannelBinding::unsupported());
        let mut message = BytesMut::new();
        frontend::sasl_initial_response(SCRAM_SHA_256, scram.message(), &mut message)
            .map_err(invalid)?;
        self.write(&message).await?;

        let Message::AuthenticationSaslContinue(body) =
            self.authentication_message(&mut budget).await?
        else {
            return Err(ProtocolError::Invalid);
        };
        scram.update(body.data()).map_err(invalid)?;
        message.clear();
        frontend::sasl_response(scram.message(), &mut message).map_err(invalid)?;
        self.write(&message).await?;

        let Message::AuthenticationSaslFinal(body) =
            self.authentication_message(&mut budget).await?
        else {
            return Err(ProtocolError::Invalid);
        };
        scram.finish(body.data()).map_err(invalid)?;
        match self.authentication_message(&mut budget).await? {
            Message::AuthenticationOk => Ok(()),
            _ => Err(ProtocolError::Invalid),
        }
    }

    async fn authentication_message(
        &mut self,
        budget: &mut ResponseBudget,
    ) -> Result<Message, ProtocolError> {
        match self.read(budget).await? {
            Message::ErrorResponse(body) => Err(ProtocolError::Database(sqlstate(body)?)),
            message => Ok(message),
        }
    }

    async fn startup_ready(&mut self) -> Result<(), ProtocolError> {
        let mut budget = ResponseBudget::default();
        loop {
            match self.authentication_message(&mut budget).await? {
                Message::ReadyForQuery(body) if body.status() == b'I' => return Ok(()),
                Message::ParameterStatus(_)
                | Message::BackendKeyData(_)
                | Message::NoticeResponse(_) => {}
                _ => return Err(ProtocolError::Invalid),
            }
        }
    }

    pub(super) async fn query(
        &mut self,
        query: &PlannedQuery<'_>,
        statement_count: usize,
    ) -> Result<QueryResult, ProtocolError> {
        let mut request = BytesMut::new();
        match query {
            PlannedQuery::Simple(sql) => frontend::query(sql, &mut request).map_err(invalid)?,
            PlannedQuery::Extended(statements) => encode_pipeline(statements, &mut request)?,
        }
        if request.len() > MAX_REQUEST {
            return Err(ProtocolError::Limit);
        }

        let start = Instant::now();
        // Read concurrently with writing so a pipelined request cannot deadlock against
        // a full worker output pipe. The owned session deadline bounds both halves.
        let (reader, mut writer) = tokio::io::split(&mut self.stream);
        let send = async {
            writer.write_all(&request).await.map_err(io_error)?;
            writer.flush().await.map_err(io_error)
        };
        let receive = receive_query(reader, start, statement_count);
        let (_, result) = tokio::try_join!(send, receive)?;
        Ok(result)
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), ProtocolError> {
        self.stream.write_all(data).await.map_err(io_error)?;
        self.stream.flush().await.map_err(io_error)
    }

    async fn read(&mut self, budget: &mut ResponseBudget) -> Result<Message, ProtocolError> {
        read_message(&mut self.stream, budget).await
    }
}

fn encode_pipeline(statements: &[&str], request: &mut BytesMut) -> Result<(), ProtocolError> {
    for sql in statements {
        frontend::parse("", sql, [], request).map_err(invalid)?;
        frontend::bind("", "", [], [], |_: (), _| Ok(IsNull::Yes), [], request)
            .map_err(|_| ProtocolError::Invalid)?;
        frontend::execute("", 0, request).map_err(invalid)?;
    }
    frontend::sync(request);
    Ok(())
}

async fn receive_query(
    mut reader: impl AsyncRead + Unpin,
    start: Instant,
    statement_count: usize,
) -> Result<QueryResult, ProtocolError> {
    let mut budget = ResponseBudget::default();
    let mut completed = Vec::new();
    let mut error = None;
    loop {
        match read_message(&mut reader, &mut budget).await? {
            Message::ReadyForQuery(body) => {
                let ready = match body.status() {
                    b'I' => Ready::Idle,
                    b'T' => Ready::Transaction,
                    b'E' => Ready::FailedTransaction,
                    _ => return Err(ProtocolError::Invalid),
                };
                if error.is_none() && completed.len() != statement_count {
                    return Err(ProtocolError::Invalid);
                }
                return Ok(QueryResult {
                    ready,
                    completed,
                    sqlstate: error,
                });
            }
            Message::CommandComplete(body)
                if error.is_none() && completed.len() < statement_count =>
            {
                completed.push(CommandCompletion {
                    kind: command_kind(body.tag().map_err(invalid)?),
                    elapsed: start.elapsed(),
                });
            }
            Message::ErrorResponse(body) if error.is_none() => error = Some(sqlstate(body)?),
            Message::BindComplete
            | Message::ParseComplete
            | Message::DataRow(_)
            | Message::RowDescription(_)
            | Message::EmptyQueryResponse
            | Message::NoticeResponse(_)
            | Message::NotificationResponse(_)
            | Message::ParameterStatus(_) => {}
            _ => return Err(ProtocolError::Invalid),
        }
    }
}

#[derive(Default)]
struct ResponseBudget {
    bytes: usize,
    messages: usize,
}

async fn read_message(
    stream: &mut (impl AsyncRead + Unpin),
    budget: &mut ResponseBudget,
) -> Result<Message, ProtocolError> {
    let mut header = [0_u8; 5];
    stream.read_exact(&mut header).await.map_err(io_error)?;
    let header = Header::parse(&header)
        .map_err(invalid)?
        .ok_or(ProtocolError::Invalid)?;
    let length = usize::try_from(header.len()).map_err(|_| ProtocolError::Invalid)? + 1;
    budget.bytes = budget.bytes.saturating_add(length);
    budget.messages += 1;
    if length > MAX_FRAME || budget.bytes > MAX_RESPONSE || budget.messages > MAX_RESPONSE_MESSAGES
    {
        return Err(ProtocolError::Limit);
    }

    let mut frame = BytesMut::zeroed(length);
    frame[0] = header.tag();
    frame[1..5].copy_from_slice(&header.len().to_be_bytes());
    stream.read_exact(&mut frame[5..]).await.map_err(io_error)?;
    Message::parse(&mut frame)
        .map_err(invalid)?
        .ok_or(ProtocolError::Invalid)
}

fn command_kind(tag: &str) -> CommandKind {
    match tag {
        "COMMIT" => CommandKind::Commit,
        "ROLLBACK" => CommandKind::Rollback,
        _ => tag
            .strip_prefix("INSERT 0 ")
            .and_then(|count| count.parse().ok())
            .map_or(CommandKind::Other, CommandKind::Insert),
    }
}

fn sqlstate(body: ErrorResponseBody) -> Result<String, ProtocolError> {
    let mut fields = body.fields();
    while let Some(field) = fields.next().map_err(invalid)? {
        if field.type_() == b'C' {
            let state = field.value_bytes();
            if state.len() == 5
                && state
                    .iter()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'A'..=b'Z'))
            {
                return std::str::from_utf8(state)
                    .map(str::to_owned)
                    .map_err(|_| ProtocolError::Invalid);
            }
            break;
        }
    }
    Err(ProtocolError::Invalid)
}

fn invalid(_: io::Error) -> ProtocolError {
    ProtocolError::Invalid
}

fn io_error(error: io::Error) -> ProtocolError {
    match error.kind() {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        io::ErrorKind::TimedOut => ProtocolError::Deadline,
        _ => ProtocolError::Transport,
    }
}
