use std::{io::Read, sync::Arc};

use anyhow::{Result, ensure};
use base64::{Engine, prelude::BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_PDF_SIZE: u64 = 50_000_000;

#[derive(Clone, Deserialize, Serialize)]
pub struct Pdf {
    name: String,
    data: Arc<str>,
}

impl Pdf {
    pub fn has_header(bytes: &[u8]) -> bool {
        bytes.starts_with(b"%PDF-")
    }

    pub fn read(name: String, reader: impl Read) -> Result<Self> {
        let mut bytes = Vec::new();
        reader.take(MAX_PDF_SIZE + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= MAX_PDF_SIZE, "PDF exceeds 50 MB");
        ensure!(Self::has_header(&bytes), "File has no PDF header");
        Ok(Self {
            name,
            data: BASE64_STANDARD.encode(bytes).into(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn encoded_len(&self) -> usize {
        self.data.len()
    }

    pub fn into_part(self) -> Value {
        json!({"inlineData": {
            "mimeType": "application/pdf",
            "displayName": self.name,
            "data": self.data.as_ref(),
        }})
    }
}
