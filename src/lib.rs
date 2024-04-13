use anyhow::Context;
use napi::bindgen_prelude::Error;
use napi_derive::napi;
use std::collections::HashMap;

// we use the actor pattern to have good cache locality
// this means that no tokenization requests will ever run in parallel, but i think that's almost certainly fine
use napi::tokio::sync::{mpsc, oneshot};

#[napi]
pub enum SupportedEncoding {
  Cl100k = 0,
}

struct TokenizerActor {
  receiver: mpsc::Receiver<TokenizerMessage>,
  cl100k_encoding: tiktoken::Encoding,
}
enum TokenizerMessage {
  ExactNumTokens {
    respond_to: oneshot::Sender<anyhow::Result<i32>>,
    text: String,
    encoding: SupportedEncoding,
    special_token_handling: tiktoken::SpecialTokenHandling,
  },
  EncodeTokens {
    respond_to: oneshot::Sender<anyhow::Result<Vec<u32>>>,
    text: String,
    encoding: SupportedEncoding,
    special_token_handling: tiktoken::SpecialTokenHandling,
  },
  // always encodes all special tokens!
  EncodeSingleToken {
    respond_to: oneshot::Sender<anyhow::Result<u32>>,
    bytes: Vec<u8>,
    encoding: SupportedEncoding,
  },
  DecodeTokens {
    respond_to: oneshot::Sender<anyhow::Result<String>>,
    tokens: Vec<u32>,
    encoding: SupportedEncoding,
  },
  DecodeTokenBytes {
    respond_to: oneshot::Sender<anyhow::Result<Vec<u8>>>,
    token: u32,
    encoding: SupportedEncoding,
  },
  ApproximateNumTokens {
    respond_to: oneshot::Sender<anyhow::Result<i32>>,
    text: String,
    encoding: SupportedEncoding,
  },
}

impl TokenizerActor {
  fn new(
    receiver: mpsc::Receiver<TokenizerMessage>,
  ) -> Result<Self, tiktoken::EncodingFactoryError> {
    let cl100k_encoding = tiktoken::EncodingFactory::cl100k_im()?;
    Ok(TokenizerActor { receiver, cl100k_encoding })
  }
  fn handle_message(&mut self, msg: TokenizerMessage) {
    match msg {
      TokenizerMessage::ExactNumTokens { respond_to, text, encoding, special_token_handling } => {
        let enc = match encoding {
          SupportedEncoding::Cl100k => &self.cl100k_encoding,
        };

        let tokens = enc.encode(&text, &special_token_handling).context("Error encoding string");

        let num_tokens = match tokens {
          Ok(t) => Ok(t.len() as i32),
          Err(e) => Err(e),
        };

        // The `let _ =` ignores any errors when sending.
        let _ = respond_to.send(num_tokens);
      }
      TokenizerMessage::EncodeTokens { respond_to, text, encoding, special_token_handling } => {
        let enc = match encoding {
          SupportedEncoding::Cl100k => &self.cl100k_encoding,
        };

        let tokens = enc.encode(&text, &special_token_handling).context("Error encoding string");

        let tokens = match tokens {
          Ok(t) => Ok(t.into_iter().map(|t| t as u32).collect()),
          Err(e) => Err(e),
        };

        // The `let _ =` ignores any errors when sending.
        let _ = respond_to.send(tokens);
      }
      TokenizerMessage::EncodeSingleToken { respond_to, bytes, encoding } => {
        let enc = match encoding {
          SupportedEncoding::Cl100k => &self.cl100k_encoding,
        };

        let token = enc.encode_single_token_bytes(&bytes);

        let token = match token {
          Ok(t) => Ok(t as u32),
          Err(_) => Err(anyhow::anyhow!("Token not recognized")),
        };

        // The `let _ =` ignores any errors when sending.
        let _ = respond_to.send(token);
      }
      TokenizerMessage::DecodeTokenBytes { respond_to, token, encoding } => {
        let enc = match encoding {
          SupportedEncoding::Cl100k => &self.cl100k_encoding,
        };
        let bytes = enc.decode_single_token_bytes(token as usize);
        let bytes = match bytes {
          Ok(b) => Ok(b),
          Err(e) => Err(anyhow::anyhow!(e)),
        };
        let _ = respond_to.send(bytes);
      }
      TokenizerMessage::DecodeTokens { respond_to, tokens, encoding } => {
        let enc = match encoding {
          SupportedEncoding::Cl100k => &self.cl100k_encoding,
        };

        let text = enc.decode(&tokens.into_iter().map(|t| t as usize).collect::<Vec<_>>());

        // The `let _ =` ignores any errors when sending.
        let _ = respond_to.send(Ok(text));
      }
      TokenizerMessage::ApproximateNumTokens { respond_to, text, encoding } => {
        let enc = match encoding {
          SupportedEncoding::Cl100k => &self.cl100k_encoding,
        };

        let tokens = enc.estimate_num_tokens_no_special_tokens_fast(&text);

        // The `let _ =` ignores any errors when sending.
        let _ = respond_to.send(Ok(tokens as i32));
      }
    }
  }
}

fn run_tokenizer_actor(mut actor: TokenizerActor) {
  while let Some(msg) = actor.receiver.blocking_recv() {
    actor.handle_message(msg);
  }
}

#[napi]
#[derive(Clone)]
pub struct Tokenizer {
  sender: mpsc::Sender<TokenizerMessage>,
}

#[napi]
pub enum SpecialTokenAction {
  /// The special token is forbidden. If it is included in the string, an error will be returned.
  Forbidden = 0,
  /// The special token is tokenized as normal text.
  NormalText = 1,
  /// The special token is treated as the special token it is. If this is applied to a specific text and the text is NOT a special token then an error will be returned. If it is the default action no error will be returned, don't worry.
  Special = 2,
}

impl SpecialTokenAction {
  pub fn to_tiktoken(&self) -> tiktoken::SpecialTokenAction {
    match self {
      SpecialTokenAction::Forbidden => tiktoken::SpecialTokenAction::Forbidden,
      SpecialTokenAction::NormalText => tiktoken::SpecialTokenAction::NormalText,
      SpecialTokenAction::Special => tiktoken::SpecialTokenAction::Special,
    }
  }
}

#[napi]
impl Tokenizer {
  pub fn new() -> Result<Self, tiktoken::EncodingFactoryError> {
    // we allow 100 outstanding requests before we fail
    // ideally we should never hit this limit... queueing up would be bad
    let (sender, receiver) = mpsc::channel(100);
    let actor = TokenizerActor::new(receiver)?;
    napi::tokio::task::spawn_blocking(move || run_tokenizer_actor(actor));

    Ok(Self { sender })
  }

  #[napi]
  pub async fn exact_num_tokens_cl100k_no_special_tokens(
    &self,
    text: String,
  ) -> Result<i32, Error> {
    let (send, recv) = oneshot::channel();
    let msg = TokenizerMessage::ExactNumTokens {
      respond_to: send,
      text,
      encoding: SupportedEncoding::Cl100k,
      special_token_handling: tiktoken::SpecialTokenHandling {
        // no special tokens!! everything is normal text
        // this is how tokenization is handled in the chat model api
        default: tiktoken::SpecialTokenAction::NormalText,
        ..Default::default()
      },
    };

    // Ignore send errors. If this send fails, so does the
    // recv.await below. There's no reason to check for the
    // same failure twice.
    let _ = self.sender.send(msg).await;
    match recv.await {
      Ok(result) => result.map_err(|e| Error::from_reason(e.to_string())),
      Err(e) => Err(Error::from_reason(format!("Actor task has been killed: {}", e.to_string()))),
    }
  }

  #[napi]
  pub async fn exact_num_tokens(
    &self,
    text: String,
    encoding: SupportedEncoding,
    special_token_default_action: SpecialTokenAction,
    special_token_overrides: HashMap<String, SpecialTokenAction>,
  ) -> Result<i32, Error> {
    let (send, recv) = oneshot::channel();
    let msg = TokenizerMessage::ExactNumTokens {
      respond_to: send,
      text,
      encoding,
      special_token_handling: tiktoken::SpecialTokenHandling {
        // no special tokens!! everything is normal text
        // this is how tokenization is handled in the chat model api
        default: special_token_default_action.to_tiktoken(),
        overrides: special_token_overrides.into_iter().map(|(k, v)| (k, v.to_tiktoken())).collect(),
      },
    };

    // Ignore send errors. If this send fails, so does the
    // recv.await below. There's no reason to check for the
    // same failure twice.
    let _ = self.sender.send(msg).await;
    match recv.await {
      Ok(result) => result.map_err(|e| Error::from_reason(e.to_string())),
      Err(e) => Err(Error::from_reason(format!("Actor task has been killed: {}", e.to_string()))),
    }
  }

  #[napi]
  pub async fn encode_cl100k_no_special_tokens(&self, text: String) -> Result<Vec<u32>, Error> {
    let (send, recv) = oneshot::channel();
    let msg = TokenizerMessage::EncodeTokens {
      respond_to: send,
      text,
      encoding: SupportedEncoding::Cl100k,
      special_token_handling: tiktoken::SpecialTokenHandling {
        // no special tokens!! everything is normal text
        // this is how tokenization is handled in the chat model api
        default: tiktoken::SpecialTokenAction::NormalText,
        ..Default::default()
      },
    };

    // Ignore send errors. If this send fails, so does the
    // recv.await below. There's no reason to check for the
    // same failure twice.
    let _ = self.sender.send(msg).await;
    match recv.await {
      Ok(result) => result.map_err(|e| Error::from_reason(e.to_string())),
      Err(e) => Err(Error::from_reason(format!("Actor task has been killed: {}", e.to_string()))),
    }
  }

  #[napi]
  pub async fn approx_num_tokens(
    &self,
    text: String,
    encoding: SupportedEncoding,
  ) -> Result<i32, Error> {
    let (send, recv) = oneshot::channel();
    let msg = TokenizerMessage::ApproximateNumTokens { respond_to: send, text, encoding };

    // Ignore send errors. If this send fails, so does the
    // recv.await below. There's no reason to check for the
    // same failure twice.
    let _ = self.sender.send(msg).await;
    match recv.await {
      Ok(result) => result.map_err(|e| Error::from_reason(e.to_string())),
      Err(e) => Err(Error::from_reason(format!("Actor task has been killed: {}", e.to_string()))),
    }
  }

  #[napi]
  pub async fn encode(
    &self,
    text: String,
    encoding: SupportedEncoding,
    special_token_default_action: SpecialTokenAction,
    special_token_overrides: HashMap<String, SpecialTokenAction>,
  ) -> Result<Vec<u32>, Error> {
    let (send, recv) = oneshot::channel();
    let msg = TokenizerMessage::EncodeTokens {
      respond_to: send,
      text,
      encoding,
      special_token_handling: tiktoken::SpecialTokenHandling {
        // no special tokens!! everything is normal text
        // this is how tokenization is handled in the chat model api
        default: special_token_default_action.to_tiktoken(),
        overrides: special_token_overrides.into_iter().map(|(k, v)| (k, v.to_tiktoken())).collect(),
      },
    };

    // Ignore send errors. If this send fails, so does the
    // recv.await below. There's no reason to check for the
    // same failure twice.
    let _ = self.sender.send(msg).await;
    match recv.await {
      Ok(result) => result.map_err(|e| Error::from_reason(e.to_string())),
      Err(e) => Err(Error::from_reason(format!("Actor task has been killed: {}", e.to_string()))),
    }
  }

  #[napi]
  pub async fn encode_single_token(
    &self,
    bytes: napi::bindgen_prelude::Uint8Array,
    encoding: SupportedEncoding,
  ) -> Result<u32, Error> {
    let (send, recv) = oneshot::channel();
    let msg =
      TokenizerMessage::EncodeSingleToken { respond_to: send, bytes: bytes.to_vec(), encoding };

    // Ignore send errors. If this send fails, so does the
    // recv.await below. There's no reason to check for the
    // same failure twice.
    let _ = self.sender.send(msg).await;
    match recv.await {
      Ok(result) => result.map_err(|e| Error::from_reason(e.to_string())),
      Err(e) => Err(Error::from_reason(format!("Actor task has been killed: {}", e.to_string()))),
    }
  }
  #[napi]
  pub async fn decode_cl100k_byte(
    &self,
    token: u32,
  ) -> Result<napi::bindgen_prelude::Uint8Array, Error> {
    let (send, recv) = oneshot::channel();
    let msg = TokenizerMessage::DecodeTokenBytes {
      respond_to: send,
      token,
      encoding: SupportedEncoding::Cl100k,
    };

    // Ignore send errors. If this send fails, so does the
    // recv.await below. There's no reason to check for the
    // same failure twice.
    let _ = self.sender.send(msg).await;
    match recv.await {
      Ok(result) => result
        .map_err(|e| napi::Error::from_reason(e.to_string()))
        .map(|v| napi::bindgen_prelude::Uint8Array::new(v.into())),
      Err(e) => Err(Error::from_reason(format!("Actor task has been killed: {}", e.to_string()))),
    }
  }

  #[napi]
  pub async fn decode_cl100k(&self, encoded_tokens: Vec<u32>) -> Result<String, Error> {
    let (send, recv) = oneshot::channel();
    let msg = TokenizerMessage::DecodeTokens {
      respond_to: send,
      tokens: encoded_tokens,
      encoding: SupportedEncoding::Cl100k,
    };

    // Ignore send errors. If this send fails, so does the
    // recv.await below. There's no reason to check for the
    // same failure twice.
    let _ = self.sender.send(msg).await;
    match recv.await {
      Ok(result) => result.map_err(|e| Error::from_reason(e.to_string())),
      Err(e) => Err(Error::from_reason(format!("Actor task has been killed: {}", e.to_string()))),
    }
  }
}

#[napi]
pub struct SyncTokenizer {
  cl100k_encoding: tiktoken::Encoding,
}

#[napi]
impl SyncTokenizer {
  #[napi(constructor)]
  pub fn new() -> Result<Self, napi::Error> {
    let cl100k_encoding = tiktoken::EncodingFactory::cl100k_im().map_err(|e| {
      napi::Error::from_reason(format!("Error creating tokenizer: {}", e.to_string()))
    })?;

    Ok(Self { cl100k_encoding })
  }

  #[napi]
  pub fn approx_num_tokens(&self, text: String, encoding: SupportedEncoding) -> Result<i32, Error> {
    let enc = match encoding {
      SupportedEncoding::Cl100k => &self.cl100k_encoding,
    };
    Ok(enc.estimate_num_tokens_no_special_tokens_fast(&text) as i32)
  }
}

#[napi]
pub fn get_tokenizer() -> Result<Tokenizer, Error> {
  Tokenizer::new().map_err(|e| Error::from_reason(e.to_string()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn test_num_tokens() {
    let tokenizer = get_tokenizer().unwrap();
    let num_tokens = tokenizer
      .exact_num_tokens_cl100k_no_special_tokens("hello, world".to_string())
      .await
      .unwrap();
    assert_eq!(num_tokens, 3);
  }
}
