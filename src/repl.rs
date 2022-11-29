use async_trait::async_trait;
use tokio::io::AsyncBufReadExt;

#[derive(Debug, PartialEq, Eq)]
pub enum HandleUserInputError {
    Terminate,
}

#[async_trait]
pub trait HandleUserInput {
    async fn handle(&mut self, user_input: String) -> Result<(), HandleUserInputError>;
}

pub struct Repl<H: HandleUserInput + Send> {
    handler: H,
    prompt: Option<String>,
}

impl<H: HandleUserInput + Send> Repl<H> {
    pub fn new(handler: H, prompt: Option<String>) -> Self {
        Self { handler, prompt }
    }
}

impl<H: HandleUserInput + Send> Repl<H> {
    pub async fn serve(&mut self) {
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        loop {
            if let Some(p) = &self.prompt {
                print!("{}", p);
            }
            std::io::Write::flush(&mut std::io::stdout()).expect("Failed to write REPL prompt.");
            if let Some(l) = lines.next_line().await.expect("Failed to get next line") {
                if self.handler.handle(l).await.is_err() {
                    break;
                }
            }
        }
    }
}
