use albatross_cli::sdk::{AgentBuilder, BackendName, SessionEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut session = AgentBuilder::new()
        .backend(BackendName::OpenAi)
        .model("gpt-5-mini")
        .builtin_tools(["file_read", "grep", "list_dir"])
        .build()?;

    let mut events = session.subscribe();
    let printer = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::Agent(albatross_cli::sdk::AgentEvent::Text { delta }) => {
                    print!("{delta}");
                }
                SessionEvent::TurnCompleted(_)
                | SessionEvent::TurnAborted(_)
                | SessionEvent::TurnFailed { .. } => break,
                _ => {}
            }
        }
    });

    let turn = session.prompt("Summarize this repository.").await?;
    printer.await?;
    println!(
        "\n\n{} input / {} output tokens",
        turn.stats.input_tokens, turn.stats.output_tokens
    );
    Ok(())
}
