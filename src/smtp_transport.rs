//! SMTP delivery using lettre. This module keeps transport diagnostics
//! deliberately generic so server responses and credentials are not exposed.

use std::{str::FromStr, time::Duration};

use lettre::{
    address::Envelope,
    message::{
        header::ContentType, Attachment as LettreAttachment, Mailbox, Message as LettreMessage,
        MultiPart, SinglePart,
    },
    transport::smtp::{
        authentication::{Credentials, Mechanism},
        client::{Tls, TlsParametersBuilder},
        Error as LettreSmtpError,
    },
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    /// SMTP over TLS from the initial connection (commonly port 465).
    Implicit,
    /// Require STARTTLS before authentication or message submission.
    StartTls,
    /// Plain SMTP. Permitted only when authentication is disabled.
    None,
}

#[derive(Clone)]
pub struct SmtpAuth {
    pub username: String,
    pub password: String,
    pub mechanism: AuthMechanism,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMechanism {
    Plain,
    Login,
}

#[derive(Clone)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub auth: Option<SmtpAuth>,
}

#[derive(Clone, Debug)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Message {
    /// RFC 5322 From header value, optionally including a display name.
    pub from: String,
    /// Optional SMTP envelope sender. Defaults to the address in `from`.
    pub envelope_from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub text: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<Attachment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// The SMTP server returned a successful final response after DATA.
    Accepted,
    /// The server returned a complete 4xx SMTP reply. Retrying may succeed.
    TransientRejection,
    /// The server returned a complete 5xx SMTP reply. Retrying the same
    /// request without changing its inputs is not expected to succeed.
    PermanentRejection,
    /// The connection failed or timed out at a point where the server may
    /// already have accepted the message. Retrying can create a duplicate.
    OutcomeUnknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    InvalidSettings,
    InvalidMessage,
    ConnectionFailed,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidSettings => "SMTP settings are invalid",
            Self::InvalidMessage => "message fields are invalid",
            Self::ConnectionFailed => "SMTP connection could not be established",
        })
    }
}

impl std::error::Error for TransportError {}

/// Test SMTP connectivity and TLS/authentication negotiation using NOOP.
pub async fn test_connection(settings: &SmtpSettings) -> Result<(), TransportError> {
    let transport = make_transport(settings)?;
    match transport.test_connection().await {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err(TransportError::ConnectionFailed),
    }
}

/// Submit one message. BCC recipients are included in the SMTP envelope and
/// intentionally never added to message headers.
pub async fn send(
    settings: &SmtpSettings,
    message: &Message,
) -> Result<SendOutcome, TransportError> {
    let transport = make_transport(settings)?;
    let (email, envelope) = build_email(message)?;
    match transport.send_raw(&envelope, &email.formatted()).await {
        Ok(_) => Ok(SendOutcome::Accepted),
        Err(error) => Ok(classify_send_error(&error)),
    }
}

fn make_transport(
    settings: &SmtpSettings,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, TransportError> {
    if settings.host.trim().is_empty()
        || settings.host.chars().any(char::is_whitespace)
        || settings.port == 0
        || (settings.tls == TlsMode::None && settings.auth.is_some())
        || settings
            .auth
            .as_ref()
            .is_some_and(|auth| auth.username.is_empty() || auth.password.is_empty())
    {
        return Err(TransportError::InvalidSettings);
    }

    let tls = match settings.tls {
        TlsMode::Implicit => Tls::Wrapper(
            TlsParametersBuilder::new(settings.host.clone())
                .build()
                .map_err(|_| TransportError::InvalidSettings)?,
        ),
        TlsMode::StartTls => Tls::Required(
            TlsParametersBuilder::new(settings.host.clone())
                .build()
                .map_err(|_| TransportError::InvalidSettings)?,
        ),
        TlsMode::None => Tls::None,
    };

    let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&settings.host)
        .port(settings.port)
        .tls(tls)
        .timeout(Some(Duration::from_secs(30)));
    if let Some(auth) = &settings.auth {
        builder = builder
            .credentials(Credentials::new(
                auth.username.clone(),
                auth.password.clone(),
            ))
            .authentication(vec![match auth.mechanism {
                AuthMechanism::Plain => Mechanism::Plain,
                AuthMechanism::Login => Mechanism::Login,
            }]);
    }
    Ok(builder.build())
}

fn build_email(message: &Message) -> Result<(LettreMessage, Envelope), TransportError> {
    let from = parse_mailbox(&message.from)?;
    if message.to.is_empty() && message.cc.is_empty() && message.bcc.is_empty() {
        return Err(TransportError::InvalidMessage);
    }
    let mut builder = LettreMessage::builder().from(from.clone());
    for address in &message.to {
        builder = builder.to(parse_mailbox(address)?);
    }
    for address in &message.cc {
        builder = builder.cc(parse_mailbox(address)?);
    }
    if message.subject.contains(['\r', '\n']) {
        return Err(TransportError::InvalidMessage);
    }

    let recipients = message
        .to
        .iter()
        .chain(message.cc.iter())
        .chain(message.bcc.iter())
        .map(|value| parse_mailbox(value).map(|mailbox| mailbox.email))
        .collect::<Result<Vec<_>, _>>()?;
    let envelope_from = match &message.envelope_from {
        Some(value) => {
            Some(lettre::Address::from_str(value).map_err(|_| TransportError::InvalidMessage)?)
        }
        None => Some(from.email),
    };
    let envelope =
        Envelope::new(envelope_from, recipients).map_err(|_| TransportError::InvalidMessage)?;

    if message.text.is_none() && message.html.is_none() {
        return Err(TransportError::InvalidMessage);
    }
    let body = match (&message.text, &message.html) {
        (Some(text), Some(html)) => MultiPart::alternative()
            .singlepart(SinglePart::plain(text.to_owned()))
            .singlepart(SinglePart::html(html.to_owned())),
        (Some(text), None) => MultiPart::mixed().singlepart(SinglePart::plain(text.to_owned())),
        (None, Some(html)) => MultiPart::mixed().singlepart(SinglePart::html(html.to_owned())),
        (None, None) => unreachable!("validated above"),
    };

    let email = if message.attachments.is_empty() {
        builder
            .subject(&message.subject)
            .multipart(body)
            .map_err(|_| TransportError::InvalidMessage)?
    } else {
        let mut mixed = match body {
            // Normalize alternative bodies as a nested mixed part. Lettre's
            // multipart builder already supports multipart nesting directly.
            multipart => MultiPart::mixed().multipart(multipart),
        };
        for attachment in &message.attachments {
            if attachment.filename.is_empty()
                || attachment.filename.contains(['\r', '\n', '/', '\\'])
            {
                return Err(TransportError::InvalidMessage);
            }
            let content_type = ContentType::parse(&attachment.content_type)
                .map_err(|_| TransportError::InvalidMessage)?;
            mixed = mixed.singlepart(
                LettreAttachment::new(attachment.filename.clone())
                    .body(attachment.bytes.clone(), content_type),
            );
        }
        builder
            .subject(&message.subject)
            .multipart(mixed)
            .map_err(|_| TransportError::InvalidMessage)?
    };
    Ok((email, envelope))
}

fn parse_mailbox(value: &str) -> Result<Mailbox, TransportError> {
    Mailbox::from_str(value).map_err(|_| TransportError::InvalidMessage)
}

fn classify_send_error(error: &LettreSmtpError) -> SendOutcome {
    // Only a parsed negative SMTP reply is definitive. Lettre does not expose
    // transaction phase for other errors, including client errors, so those
    // stay unknown to avoid reporting a safe retry after DATA may have begun.
    classify_send_error_flags(error.is_transient(), error.is_permanent())
}

fn classify_send_error_flags(transient: bool, permanent: bool) -> SendOutcome {
    if transient {
        SendOutcome::TransientRejection
    } else if permanent {
        SendOutcome::PermanentRejection
    } else {
        SendOutcome::OutcomeUnknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_message() -> Message {
        Message {
            from: "Sender <sender@example.test>".into(),
            envelope_from: None,
            to: vec!["to@example.test".into()],
            cc: vec!["Copy <cc@example.test>".into()],
            bcc: vec!["hidden@example.test".into()],
            subject: "Hello".into(),
            text: Some("plain body".into()),
            html: Some("<p>html body</p>".into()),
            attachments: vec![Attachment {
                filename: "note.txt".into(),
                content_type: "text/plain; charset=utf-8".into(),
                bytes: b"attachment".to_vec(),
            }],
        }
    }

    #[test]
    fn bcc_is_envelope_only_and_attachments_are_mime_parts() {
        let (email, envelope) = build_email(&sample_message()).unwrap();
        let wire = String::from_utf8(email.formatted()).unwrap();
        assert!(wire.contains("to@example.test"));
        assert!(wire.contains("cc@example.test"));
        assert!(!wire.to_ascii_lowercase().contains("bcc:"));
        assert!(!wire.contains("hidden@example.test"));
        assert!(wire.contains("note.txt"));
        assert_eq!(envelope.to().len(), 3);
    }

    #[test]
    fn rejects_authentication_without_tls() {
        let settings = SmtpSettings {
            host: "mail.example.test".into(),
            port: 25,
            tls: TlsMode::None,
            auth: Some(SmtpAuth {
                username: "user".into(),
                password: "secret".into(),
                mechanism: AuthMechanism::Plain,
            }),
        };
        assert!(matches!(
            make_transport(&settings),
            Err(TransportError::InvalidSettings)
        ));
    }

    #[test]
    fn only_a_complete_negative_smtp_reply_is_definite() {
        assert_eq!(
            classify_send_error_flags(true, false),
            SendOutcome::TransientRejection
        );
        assert_eq!(
            classify_send_error_flags(false, true),
            SendOutcome::PermanentRejection
        );
        assert_eq!(
            classify_send_error_flags(false, false),
            SendOutcome::OutcomeUnknown
        );
    }
}
