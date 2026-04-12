use futures::StreamExt;
use mirage_core::{
    VeniceAgent,
    agent::{MultiTurnStreamItem, Text},
    message::Message,
    session::{StreamEvent, summarize_tool_call},
    streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt},
};
use tokio::sync::mpsc;

pub(crate) async fn stream_agent_response(
    agent: VeniceAgent,
    prompt: String,
    history: Vec<Message>,
    max_turns: usize,
    tx: mpsc::UnboundedSender<StreamEvent>,
) {
    let mut stream = agent
        .stream_prompt(prompt)
        .with_history(history)
        .multi_turn(max_turns)
        .await;

    while let Some(item) = stream.next().await {
        let event = match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(
                Text { text },
            ))) => StreamEvent::AssistantText(text),
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
                tool_call,
                ..
            })) => {
                let name = tool_call.function.name;
                let summary = summarize_tool_call(&name, &tool_call.function.arguments);
                StreamEvent::ToolCall {
                    id: tool_call.id,
                    name,
                    summary,
                }
            }
            Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                ..
            })) => StreamEvent::ToolResult { id: tool_result.id },
            Ok(MultiTurnStreamItem::FinalResponse(final_response)) => {
                StreamEvent::Final(final_response)
            }
            Ok(_) => continue,
            Err(error) => StreamEvent::Error(error.to_string()),
        };

        let is_terminal = matches!(event, StreamEvent::Final(_) | StreamEvent::Error(_));
        if tx.send(event).is_err() {
            break;
        }
        if is_terminal {
            break;
        }
    }
}
