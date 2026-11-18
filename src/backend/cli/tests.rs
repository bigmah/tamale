use serde_json::json;

use super::*;

#[test]
fn replays_earlier_conversation_without_the_new_prompt() {
    let mut history = vec![
        Message::user("first"),
        Message::Assistant {
            id: None,
            content: vec![
                AssistantContent::text("checking"),
                AssistantContent::tool_call("t1", "read_file", json!({"path": "a.rs"})),
            ],
        },
        Message::User {
            content: vec![UserContent::tool_result_from_wire(
                "t1",
                "read_file",
                vec![ToolResultContent::text("x".repeat(SEED_RESULT_CHARS + 10))],
            )],
        },
    ];
    agent::push_user_text(&mut history, "next".into());

    let prior = without_prompt(&history, "next");
    assert_eq!(prior.len(), 3);
    let Message::User { content } = &prior[2] else {
        panic!("tool results stay");
    };
    assert_eq!(content.len(), 1);

    let prompt = seed(&prior, "next");
    assert!(prompt.starts_with("<previous_conversation>"));
    assert!(prompt.contains("[user]\nfirst\n"));
    assert!(prompt.contains("[assistant]\nchecking\n"));
    assert!(prompt.contains(r#"[tool call] read_file {"path":"a.rs"}"#));
    assert!(prompt.contains("… [shortened]"));
    assert!(prompt.ends_with("</previous_conversation>\n\nnext"));

    assert_eq!(without_prompt(&[Message::user("only")], "only").len(), 0);
    assert_eq!(seed(&[], "only"), "only");
}

#[cfg(unix)]
#[tokio::test]
async fn a_run_reads_the_prompt_and_reports_each_json_line_then_how_it_ended() {
    let mut command = Command::new("sh");
    command.args([
        "-c",
        r#"read -r prompt; echo "not json"; echo "{\"said\":\"$prompt\"}"; echo "went wrong" >&2; exit 3"#,
    ]);
    let mut events = Vec::new();
    let exit = stream(command, &std::env::temp_dir(), "hello\n".into(), |event| {
        events.push(event)
    })
    .await
    .unwrap();
    assert_eq!(events, [json!({"said": "hello"})]);
    assert_eq!(
        exit.unfinished("The CLI").await.to_string(),
        "The CLI exited (exit status: 3) before finishing the response: went wrong"
    );
}
