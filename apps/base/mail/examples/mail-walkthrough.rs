//! Manual mail walkthrough.
//! Uses only example.test addresses and prints no credentials or tokens.
use bilibi_mail::{render, Address, Attachment, Limits, ListUnsubscribe, Mailer, Message};
use std::io::Write;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let also_smtp = args.iter().any(|arg| arg == "--also-smtp");
    let mailer = if also_smtp {
        Mailer::from_lookup(|key| {
            (key != "MAIL_HOST")
                .then(|| std::env::var(key).ok())
                .flatten()
        })?
    } else {
        Mailer::from_env()?
    };
    let sender = Address::new(Some("Support"), "support@example.test")?;
    let mut message = Message::text(
        Address::new(Some("Customer"), "customer@example.test")?,
        "Your statement · October",
        "Your statement is attached.\nhttps://example.test/a?long=one&value=two\n",
    );
    message.from = Some(sender.clone());
    message.reply_to = Some(Address::new(None, "replies@example.test")?);
    message.cc.push(Address::new(None, "copy@example.test")?);
    message
        .bcc
        .push(Address::new(None, "private@example.test")?);
    message.html = Some("<p>Your <strong>statement</strong> is attached.</p>".into());
    message.attachments = vec![
        Attachment {
            filename: "statement.pdf".into(),
            content_type: "application/pdf".into(),
            bytes: b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\n%%EOF\n".to_vec(),
        },
        Attachment {
            filename: "notes.txt".into(),
            content_type: "text/plain".into(),
            bytes: b"October statement notes\n".to_vec(),
        },
    ];
    message.message_id = Some("<statement-october@example.test>".into());
    message.in_reply_to = Some("<request@example.test>".into());
    message.references = vec![
        "<case@example.test>".into(),
        "<request@example.test>".into(),
    ];
    message.auto_submitted = Some("auto-replied".into());
    message.list_unsubscribe = Some(ListUnsubscribe {
        urls: "<https://example.test/unsubscribe>".into(),
        one_click: true,
    });
    message
        .extra_headers
        .push(("X-Case-ID".into(), "case-123".into()));
    let rendered = mailer.render(&message).await?;
    println!(
        "transport: {}; dev mailbox: {}",
        mailer.describe(),
        mailer.dev_mailbox().is_some()
    );
    println!(
        "envelope: {} -> {:?}",
        rendered.envelope.from, rendered.envelope.recipients
    );
    // Pass this very Rendered to a second transport: no MIME rebuild or new Date.
    if let Some(path) = args.iter().find(|arg| !arg.starts_with("--")) {
        std::fs::write(path, &rendered.bytes)?;
    }
    let sent = mailer.deliver_rendered(rendered.clone()).await?;
    println!(
        "sent id: {}; exact rendered bytes returned: {}",
        sent.message_id,
        sent.bytes == rendered.bytes
    );
    if also_smtp {
        let smtp = Mailer::from_env()?;
        if smtp.dev_mailbox().is_some() {
            return Err("--also-smtp needs MAIL_HOST".into());
        }
        let replay = smtp.deliver_rendered(rendered.clone()).await?;
        println!(
            "SMTP dev mailbox: {}; same Rendered bytes returned: {}",
            smtp.dev_mailbox().is_some(),
            replay.bytes == sent.bytes
        );
    }
    std::io::stdout().write_all(&sent.bytes)?;
    println!("\n== refused input ==");
    let show = |label, result: bilibi_mail::Result<bilibi_mail::Rendered>| {
        println!(
            "{label}: {}",
            if result.is_err() {
                "refused"
            } else {
                "ACCEPTED"
            }
        );
    };
    let limits = Limits::default();
    let mut invalid = message.clone();
    invalid.to.name = Some("Customer\r\nBcc: attacker@example.test".into());
    show("name injection", render(&invalid, &sender, limits));
    invalid = message.clone();
    invalid.subject.push_str("\nBcc: attacker@example.test");
    show("subject injection", render(&invalid, &sender, limits));
    invalid = message.clone();
    invalid.extra_headers = vec![("X-Case".into(), "case\r\nBcc: attacker@example.test".into())];
    show("extra header injection", render(&invalid, &sender, limits));
    invalid.extra_headers = vec![("From".into(), "attacker@example.test".into())];
    show("reserved header", render(&invalid, &sender, limits));
    show(
        "encoded size limit",
        render(
            &message,
            &sender,
            Limits {
                max_message_bytes: 1_000,
                ..limits
            },
        ),
    );
    show(
        "recipient limit including Bcc",
        render(
            &message,
            &sender,
            Limits {
                max_recipients: 2,
                ..limits
            },
        ),
    );
    invalid = message.clone();
    invalid.message_id = Some("<bad id@example.test>".into());
    show("invalid message id", render(&invalid, &sender, limits));
    invalid = message.clone();
    invalid.message_id = None;
    let generated = render(&invalid, &sender, limits)?;
    println!(
        "generated id uses sender domain: {}",
        generated.message_id.ends_with("@example.test>")
    );
    if let Some(mailbox) = mailer.dev_mailbox() {
        println!(
            "stored messages: {}; inbound/ is not a mailbox entry",
            mailbox.entries()?.len()
        );
    }
    Ok(())
}
