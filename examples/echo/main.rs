//! The smallest module: a free, a paid and a long operation, and an MCP App
//! that shows what was said, served over stdio.
//!
//! The kit's own tests use it, so it stays a working example.

use std::sync::Arc;

use pamin_kit::schemars::JsonSchema;
use pamin_kit::{
    Audience, Body, Ctx, Feature, Hardware, Hints, Kit, Local, Manifest, Message, Module, Ops,
    Role, Routing, Space,
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

/// Text to say, as a resource: what reading it would say back.
#[derive(Deserialize, JsonSchema)]
pub struct SayUri {
    /// What to say.
    text: String,
}

/// Asks the user before saying the text back.
#[derive(Deserialize, JsonSchema)]
pub struct Confirm {
    /// What to say, once the user agrees.
    text: String,
}

/// The user's answer to whether to say it.
#[derive(Deserialize, JsonSchema)]
pub struct Sure {
    /// Say it?
    yes: bool,
}

/// Starts a conversation about something to say.
#[derive(Deserialize, JsonSchema)]
pub struct Greet {
    /// Who to greet.
    name: String,
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

    /// Says the text back only once the user agrees.
    async fn confirm(&self, ctx: &Ctx, confirm: Confirm) -> Result<Said, String> {
        let question = format!("Say {:?}?", confirm.text);
        match ctx.ask::<Sure>("sure", &question)? {
            Some(Sure { yes: true }) => self.say(ctx.space(), Say { text: confirm.text }).await,
            _ => Err("the user did not want it said".into()),
        }
    }

    /// Waits in ten steps, reporting each, and stops early when cancelled.
    async fn wait(&self, ctx: &Ctx, wait: Wait) -> Result<Said, String> {
        const STEPS: u64 = 10;
        for step in 0..STEPS {
            if ctx.is_cancelled() {
                return Err("cancelled".into());
            }
            ctx.progress(step as f64, Some(STEPS as f64), Some("waiting"))
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(wait.millis / STEPS)).await;
        }
        self.say(ctx.space(), Say { text: wait.text }).await
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
            |m: Arc<Self>, ctx: Ctx, a: Say| async move {
                let said = m.say(ctx.space(), a).await?;
                // What reading echo://say/{text} gives has just been said.
                ctx.updated(format!("echo://say/{}", said.text));
                Ok::<_, String>(said)
            },
        )
        .tool(
            "echo_shout",
            Feature::Pro("shout"),
            |m: Arc<Self>, ctx: Ctx, a: Say| async move { m.say(ctx.space(), a).await },
        )
        .tool(
            "echo_confirm",
            Feature::Core,
            |m: Arc<Self>, ctx: Ctx, a: Confirm| async move { m.confirm(&ctx, a).await },
        )
        .task(
            "echo_wait",
            Feature::Core,
            |m: Arc<Self>, ctx: Ctx, a: Wait| async move { m.wait(&ctx, a).await },
        )
        .app("echo_say", "ui://echo/said", include_str!("said.html"))
        .hints(
            "echo_say",
            Hints {
                title: Some("Say"),
                read_only: true,
                idempotent: true,
                open_world: false,
                ..Hints::default()
            },
        )
        .resource(
            "echo://about",
            "About echo",
            Feature::Core,
            |_: Arc<Self>, _: Ctx, (): ()| async {
                Ok::<_, String>(Body::Text {
                    mime: "text/plain",
                    text: "Echo says text back.".into(),
                })
            },
        )
        .resource(
            "echo://say/{text}",
            "Said",
            Feature::Core,
            |m: Arc<Self>, ctx: Ctx, uri: SayUri| async move {
                let said = m.say(ctx.space(), Say { text: uri.text }).await?;
                let text = serde_json::to_string(&said).map_err(|error| error.to_string())?;
                Ok::<_, String>(Body::Text {
                    mime: "application/json",
                    text,
                })
            },
        )
        .prompt(
            "echo_greet",
            Feature::Core,
            |_: Arc<Self>, _: Ctx, greet: Greet| async move {
                Ok::<_, String>(vec![Message {
                    role: Role::User,
                    text: format!("Say hello to {} with echo_say.", greet.name),
                }])
            },
        )
        .complete(
            "echo_greet",
            "name",
            |_: Arc<Self>, _: Ctx, typed: String| async move {
                let names = ["Ada", "Alan", "Grace"];
                Ok::<_, String>(
                    names
                        .iter()
                        .filter(|name| name.starts_with(&typed))
                        .map(|name| name.to_string())
                        .collect(),
                )
            },
        )
        .instructions("Use echo_say to hear text back; echo_wait for a slow reply.");
    }
}

#[allow(dead_code)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    Kit::new(Echo, Local::new("example")).serve_stdio().await
}
