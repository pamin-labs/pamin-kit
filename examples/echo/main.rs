//! The smallest module: a free, a paid and a long operation, served over
//! stdio.
//!
//! The kit's own tests use it, so it stays a working example.

use std::sync::Arc;

use pamin_kit::schemars::JsonSchema;
use pamin_kit::{
    Audience, Ctx, Feature, Hardware, Kit, Local, Manifest, Module, Ops, Routing, Space,
};
use serde::{Deserialize, Serialize};

pub struct Echo;

/// Says the text back, with the project it was said in.
#[derive(Deserialize, JsonSchema)]
pub struct Say {
    /// What to say.
    text: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Said {
    text: String,
    project: String,
}

/// Waits, then says the text back.
#[derive(Deserialize, JsonSchema)]
pub struct Wait {
    /// What to say.
    text: String,
    /// How long to wait first, in milliseconds.
    millis: u64,
}

impl Echo {
    async fn say(&self, space: &Space, say: Say) -> Result<Said, String> {
        if say.text.is_empty() {
            return Err("nothing to say".into());
        }
        Ok(Said {
            text: say.text,
            project: space.project.clone(),
        })
    }

    async fn wait(&self, space: &Space, wait: Wait) -> Result<Said, String> {
        tokio::time::sleep(std::time::Duration::from_millis(wait.millis)).await;
        self.say(space, Say { text: wait.text }).await
    }
}

impl Module for Echo {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: "echo",
            contract_version: "1.0.0",
            routing: Routing::Stateless,
            long_running: true,
            scopes: vec![],
            metering_unit: "call",
            hardware: Hardware::default(),
            audience: Audience::default(),
        }
    }

    fn register(&self, ops: &mut Ops<Self>) {
        ops.tool(
            "echo_say",
            Feature::Core,
            |m: Arc<Self>, ctx: Ctx, a: Say| async move { m.say(ctx.space(), a).await },
        )
        .tool(
            "echo_shout",
            Feature::Pro("shout"),
            |m: Arc<Self>, ctx: Ctx, a: Say| async move { m.say(ctx.space(), a).await },
        )
        .task(
            "echo_wait",
            Feature::Core,
            |m: Arc<Self>, ctx: Ctx, a: Wait| async move { m.wait(ctx.space(), a).await },
        );
    }
}

#[allow(dead_code)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    Kit::new(Echo, Local::new("example")).serve_stdio().await
}
