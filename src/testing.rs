//! Test doubles shared by integration tests.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::things::{RawReply, RawRequest, Transport};

type Handler = Box<dyn Fn(&RawRequest) -> RawReply + Send + Sync>;

/// A transport backed by a closure; records every request it receives.
pub struct FakeTransport {
    handler: Handler,
    requests: Mutex<Vec<RawRequest>>,
}

impl FakeTransport {
    pub fn new<F>(handler: F) -> Arc<Self>
    where
        F: Fn(&RawRequest) -> RawReply + Send + Sync + 'static,
    {
        Arc::new(Self {
            handler: Box::new(handler),
            requests: Mutex::new(Vec::new()),
        })
    }

    pub fn requests(&self) -> Vec<RawRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    pub fn urls(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|request| request.url)
            .collect()
    }
}

#[async_trait]
impl Transport for FakeTransport {
    async fn execute(&self, request: RawRequest) -> anyhow::Result<RawReply> {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        Ok((self.handler)(&request))
    }
}
