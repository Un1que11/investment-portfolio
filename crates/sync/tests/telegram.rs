//! The Bot API client against a local mock (never the real Telegram). The token is fake.

use portfolio_sync::telegram::{TelegramClient, TelegramError};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "123456:TEST-token";

#[tokio::test]
async fn sends_plain_text_to_the_chat() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/sendMessage")))
        .and(body_string_contains("chat_id=-1001"))
        // "Portfel · 1 000,00 zł" form-encoded: spaces as "+", "·" and NBSP as UTF-8 bytes.
        .and(body_string_contains(
            "text=Portfel+%C2%B7+1%C2%A0000%2C00%C2%A0z%C5%82",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"ok":true,"result":{"message_id":7}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = TelegramClient::new(server.uri(), TOKEN, "-1001").unwrap();
    client
        .send_message("Portfel · 1\u{a0}000,00\u{a0}zł")
        .await
        .unwrap();
}

#[tokio::test]
async fn a_refusal_is_reported_by_its_description() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#,
        ))
        .mount(&server)
        .await;
    let client = TelegramClient::new(server.uri(), TOKEN, "-1").unwrap();
    let err = client.send_message("hi").await.unwrap_err();
    assert!(matches!(err, TelegramError::Api { status: 400, .. }));
    assert_eq!(
        err.to_string(),
        "Telegram answered HTTP 400: Bad Request: chat not found"
    );
}

#[tokio::test]
async fn ok_false_or_an_unreadable_body_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>proxy</html>"))
        .mount(&server)
        .await;
    let client = TelegramClient::new(server.uri(), TOKEN, "-1").unwrap();
    assert_eq!(
        client.send_message("hi").await.unwrap_err().to_string(),
        "Telegram answered HTTP 200: no description"
    );
}

#[tokio::test]
async fn the_token_never_shows_in_errors_or_debug() {
    // Nothing listens on port 9 here: the transport error would carry the URL with the token.
    let client = TelegramClient::new("http://127.0.0.1:9", TOKEN, "-1001").unwrap();
    let err = client.send_message("hi").await.unwrap_err();
    assert!(matches!(err, TelegramError::Http(_)));
    for text in [err.to_string(), format!("{err:?}"), format!("{client:?}")] {
        assert!(!text.contains(TOKEN), "{text}");
        assert!(!text.contains("123456"), "{text}");
    }
    assert!(!format!("{client:?}").contains("-1001"));

    // An error description that echoes the token is redacted too.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string(format!(
            r#"{{"ok":false,"description":"Unauthorized: {TOKEN}"}}"#
        )))
        .mount(&server)
        .await;
    let client = TelegramClient::new(server.uri(), TOKEN, "-1").unwrap();
    assert_eq!(
        client.send_message("hi").await.unwrap_err().to_string(),
        "Telegram answered HTTP 401: Unauthorized: <redacted>"
    );
}
