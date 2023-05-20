use std::{fs, path::PathBuf, sync::Arc, time::Duration};

use jmap::{JMAP, SUPERUSER_ID};
use jmap_client::{
    client::Client,
    core::set::{SetError, SetErrorType},
    email, mailbox,
    sieve::query::{Comparator, Filter},
    Error,
};
use jmap_proto::types::id::Id;

use crate::jmap::{
    delivery::SmtpConnection,
    email_submission::{assert_message_delivery, spawn_mock_smtp_server, MockMessage},
    test_account_create,
};
use crate::smtp::session::VerifyResponse;

pub async fn test(server: Arc<JMAP>, client: &mut Client) {
    println!("Running Sieve tests...");

    // Create test account
    let account_id = test_account_create(&server, "jdoe@example.com", "12345", "John Doe")
        .await
        .to_string();
    client.set_default_account_id(&account_id);

    // Validate scripts
    client
        .sieve_script_validate(get_script("validate_ok"))
        .await
        .unwrap();
    assert!(matches!(
        client
            .sieve_script_validate(get_script("validate_error"))
            .await,
        Err(Error::Set(SetError {
            type_: SetErrorType::InvalidScript,
            ..
        }))
    ));

    // Create 5 Sieve scripts, all deactivated.
    let mut script_ids = Vec::new();
    for i in 0..5 {
        script_ids.push(
            client
                .sieve_script_create(
                    format!("script_{}", i + 1),
                    format!("require \"fileinto\"; fileinto \"{}\";", i + 1).into_bytes(),
                    false,
                )
                .await
                .unwrap()
                .take_id(),
        );
    }

    let response = client
        .sieve_script_query(Filter::is_active(false).into(), [Comparator::name()].into())
        .await
        .unwrap();
    assert_eq!(response.ids().len(), 5);
    for (pos, id) in response.ids().iter().enumerate() {
        let script = client
            .sieve_script_get(id, None::<Vec<_>>)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(script.name().unwrap(), format!("script_{}", pos + 1));
        assert_eq!(
            String::from_utf8(client.download(script.blob_id().unwrap()).await.unwrap()).unwrap(),
            format!("require \"fileinto\"; fileinto \"{}\";", pos + 1)
        );
    }

    // Activate last script twice and then the first script
    for _ in 0..2 {
        client
            .sieve_script_activate(script_ids.last().unwrap())
            .await
            .unwrap();
        assert_eq!(
            client
                .sieve_script_query(Filter::is_active(true).into(), [Comparator::name()].into())
                .await
                .unwrap()
                .ids(),
            vec![script_ids.last().unwrap().to_string()]
        );
    }
    client
        .sieve_script_activate(script_ids.first().unwrap())
        .await
        .unwrap();
    assert_eq!(
        client
            .sieve_script_query(Filter::is_active(true).into(), [Comparator::name()].into())
            .await
            .unwrap()
            .ids(),
        vec![script_ids.first().unwrap().to_string()]
    );

    // Destroying an active script should not work
    assert!(matches!(
        client
            .sieve_script_destroy(script_ids.first().unwrap())
            .await,
        Err(Error::Set(SetError {
            type_: SetErrorType::ScriptIsActive,
            ..
        }))
    ));

    // Deactivate all scripts
    client.sieve_script_deactivate().await.unwrap();
    assert_eq!(
        client
            .sieve_script_query(Filter::is_active(true).into(), [Comparator::name()].into())
            .await
            .unwrap()
            .ids(),
        Vec::<String>::new()
    );

    // Connect to LMTP service
    let mut lmtp = SmtpConnection::connect().await;

    // Run mailbox, fileinto, flags tests
    client
        .sieve_script_create("test_mailbox", get_script("test_mailbox"), true)
        .await
        .unwrap();
    lmtp.ingest(
        "bill@example.com",
        &["jdoe@example.com"],
        concat!(
            "From: bill@example.com\r\n",
            "To: jdoe@example.com\r\n",
            "Subject: TPS Report\r\n",
            "\r\n",
            "I'm going to need those TPS reports ASAP. ",
            "So, if you could do that, that'd be great."
        ),
    )
    .await;

    // Make sure all folders were created
    let mailbox_names = "My/Nested/Mailbox/with/multiple/levels/Folder"
        .split('/')
        .collect::<Vec<_>>();
    let mut mailbox_ids = Vec::new();
    for &mailbox in &mailbox_names {
        let mut response = client
            .mailbox_query(mailbox::query::Filter::name(mailbox).into(), None::<Vec<_>>)
            .await
            .unwrap();
        assert!(
            !response.ids().is_empty(),
            "Mailbox {} was not created.",
            mailbox
        );
        mailbox_ids.extend(response.take_ids());
    }
    assert_eq!(mailbox_ids.len(), mailbox_names.len());

    // Make sure the message was delivered to the right folders
    let message_ids = client
        .email_query(None::<email::query::Filter>, None::<Vec<_>>)
        .await
        .unwrap()
        .take_ids();
    assert_eq!(message_ids.len(), 1, "too many messages {:?}", message_ids);
    let email = client
        .email_get(
            message_ids.last().unwrap(),
            [email::Property::MailboxIds, email::Property::Keywords].into(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        email.keywords().len(),
        2,
        "Expected 2 keywords, found {:?}.",
        email.keywords()
    );
    for keyword in ["$important", "$seen"] {
        if !email.keywords().contains(&keyword) {
            panic!("Keyword {} not found in {:?}.", keyword, email.keywords());
        }
    }
    assert_eq!(
        email.mailbox_ids().len(),
        2,
        "Expected 2 mailbox ids, found {:?}.",
        email.mailbox_ids()
    );
    for mailbox_pos in [mailbox_ids.len() - 1, mailbox_ids.len() - 2] {
        if !email
            .mailbox_ids()
            .contains(&mailbox_ids[mailbox_pos].as_str())
        {
            panic!(
                "Mailbox {} ({}) not found in {:?}.",
                mailbox_names[mailbox_pos],
                mailbox_ids[mailbox_pos],
                email.keywords()
            );
        }
    }

    // Run discard and duplicate tests
    client
        .sieve_script_create(
            "test_discard_reject",
            get_script("test_discard_reject"),
            true,
        )
        .await
        .unwrap();
    lmtp.ingest(
        "bill@example.com",
        &["jdoe@example.com"],
        concat!(
            "From: bill@example.com\r\n",
            "Bcc: Undisclosed recipients;\r\n",
            "Message-ID: <1234@example.com>\r\n",
            "Subject: Holidays\r\n",
            "\r\n",
            "Remember to file your TPS reports before ",
            "going on holidays."
        ),
    )
    .await;
    assert_eq!(
        client
            .email_query(None::<email::query::Filter>, None::<Vec<_>>)
            .await
            .unwrap()
            .ids()
            .len(),
        1,
        "Discard failed."
    );

    // Let one sec duplicate ids expire
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // Run reject and duplicate check tests
    let test = "fd";
    /*lmtp.ingest_with_code(
        "bill@example.com",
        &["jdoe@example.com"],
        concat!(
            "From: bill@example.com\r\n",
            "Bcc: Undisclosed recipients;\r\n",
            "Message-ID: <1234@example.com>\r\n",
            "Subject: Holidays\r\n",
            "\r\n",
            "Remember to file your T.P.S. reports before ",
            "going on holidays."
        ),
        5,
    )
    .await
    .assert_contains("No soup for you");
    assert_eq!(
        client
            .email_query(None::<email::query::Filter>, None::<Vec<_>>)
            .await
            .unwrap()
            .ids()
            .len(),
        1,
        "Reject failed."
    );*/

    // Run include tests
    client
        .sieve_script_create("test_include_this", get_script("test_include_this"), false)
        .await
        .unwrap();
    client
        .sieve_script_create("test_include", get_script("test_include"), true)
        .await
        .unwrap();
    lmtp.ingest_with_code(
        "bill@example.com",
        &["jdoe@example.com"],
        concat!(
            "From: bill@example.com\r\n",
            "Bcc: Undisclosed recipients;\r\n",
            "Message-ID: <1234@example.com>\r\n",
            "Subject: Holidays\r\n",
            "\r\n",
            "Remember to file your T.P.S. reports before ",
            "going on holidays."
        ),
        5,
    )
    .await
    .assert_contains("Rejected from an included script");

    // Start mock SMTP server
    let coco = "fd";
    /*let (mut smtp_rx, smtp_settings) = spawn_mock_smtp_server();

    // Run enclose + redirect tests
    client
        .sieve_script_create(
            "test_redirect_enclose",
            get_script("test_redirect_enclose"),
            true,
        )
        .await
        .unwrap();
    lmtp.ingest(
        "bill@example.com",
        &["jdoe@example.com"],
        concat!(
            "From: bill@example.com\r\n",
            "To: jdoe@example.com\r\n",
            "Subject: TPS Report\r\n",
            "\r\n",
            "I'm going to need those TPS reports ASAP. ",
            "So, if you could do that, that'd be great."
        ),
    )
    .await;
    assert_message_delivery(
        &mut smtp_rx,
        MockMessage::new(
            "<jdoe@example.com>",
            ["<jane@example.com>"],
            "@Attached you'll find",
        ),
        false,
    )
    .await;
    assert_eq!(
        client
            .email_query(None::<email::query::Filter>, None::<Vec<_>>)
            .await
            .unwrap()
            .ids()
            .len(),
        1,
        "Redirected message was stored."
    );

    // Run notify + editheader + notify + fcc tests
    client
        .sieve_script_create("test_notify_fcc", get_script("test_notify_fcc"), true)
        .await
        .unwrap();
    lmtp.ingest(
        "bill@example.com",
        &["jdoe@example.com"],
        concat!(
            "From: bill@example.com\r\n",
            "To: jdoe@example.com\r\n",
            "Subject: Urgently I need those TPS Reports\r\n",
            "\r\n",
            "I'm going to need those TPS reports ASAP. ",
            "So, if you could do that, that'd be great."
        ),
    )
    .await;

    assert_message_delivery(
        &mut smtp_rx,
        MockMessage::new(
            "<jdoe@example.com>",
            ["<sms_gateway@example.com>"],
            "@It's TPS-o-clock",
        ),
        false,
    )
    .await;

    let mut request = client.build();
    request.get_email().properties([
        email::Property::MailboxIds,
        email::Property::Keywords,
        email::Property::Subject,
    ]);
    let emails = request.send_get_email().await.unwrap().take_list();

    assert_eq!(
        emails.len(),
        3,
        "Two new messages were expected: {:#?}.",
        emails
    );

    'outer: for (subject, folder, keywords) in [
        ("It's TPS-o-clock", "Notifications", ""),
        (
            "Urgently I need those **censored** Reports",
            "Inbox",
            "$seen",
        ),
    ] {
        for email in &emails {
            if email.subject().unwrap().eq(subject) {
                if !keywords.is_empty() && !email.keywords().contains(&keywords) {
                    panic!("Keyword {:?} not found in: {:#?}", keywords, email);
                }

                let mailbox_id = client
                    .mailbox_query(
                        mailbox::query::Filter::name(folder.to_string()).into(),
                        None::<Vec<_>>,
                    )
                    .await
                    .unwrap()
                    .take_ids()
                    .pop()
                    .unwrap_or_else(|| panic!("Mailbox {:?} not found", folder));

                if !email.mailbox_ids().contains(&mailbox_id.as_str()) {
                    panic!(
                        "Mailbox {:?} ({}) not found in: {:#?}",
                        folder, mailbox_id, email
                    );
                }

                continue 'outer;
            }
        }
        panic!("Email {:?} not found in: {:#?}", subject, emails);
    }

    smtp_settings.lock().do_stop = true;

    */

    // Remove test data
    let todo = "fix";
    /*for account_id in [&account_id, &domain_id] {
        client
            .set_default_account_id(Id::new(SUPERUSER_ID as u64))
            .principal_destroy(account_id)
            .await
            .unwrap();
    }
    server.store.principal_purge().unwrap();
    server.store.assert_is_empty();*/
}

fn get_script(name: &str) -> Vec<u8> {
    let mut script_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    script_path.push("resources");
    script_path.push("jmap_sieve");
    script_path.push(format!("{}.sieve", name));
    fs::read(script_path).unwrap()
}
