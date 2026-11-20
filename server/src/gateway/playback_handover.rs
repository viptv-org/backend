use super::*;

const HANDOVER: Duration = Duration::from_secs(3);
const RETRY_PAUSE: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, PartialEq)]
enum Hold {
    Idle,
    Held,
    Released(Instant),
}

#[derive(Clone)]
pub(super) struct Output(Arc<tokio::sync::watch::Sender<Hold>>);
impl Output {
    pub(super) fn new() -> Self {
        Self(Arc::new(tokio::sync::watch::channel(Hold::Idle).0))
    }
    pub(super) fn released(&self) {
        self.0.send_replace(Hold::Released(Instant::now()));
    }
    fn hold(&self) {
        self.0.send_replace(Hold::Held);
    }
    fn abandon(&self) {
        self.0.send_if_modified(|hold| {
            let held = *hold == Hold::Held;
            if held {
                *hold = Hold::Idle;
            }
            held
        });
    }
    fn pending(&self) -> bool {
        match *self.0.borrow() {
            Hold::Idle => false,
            Hold::Held => true,
            Hold::Released(at) => at.elapsed() < HANDOVER,
        }
    }
    async fn settled(self) {
        let mut hold = self.0.subscribe();
        let _ = hold.wait_for(|hold| *hold != Hold::Held).await;
    }
}

fn replaced_outputs(registry: &Registry, entry: &Entry) -> Vec<Output> {
    registry
        .entries
        .lock()
        .unwrap()
        .values()
        .filter(|other| {
            other.id != entry.id
                && other.scope == entry.scope
                && other.cancelled.load(Ordering::Acquire)
                && (other.identity == entry.identity
                    || viewing::same_target(&other.proof, &entry.proof))
                && other.output.pending()
        })
        .map(|other| other.output.clone())
        .collect()
}

pub(super) async fn create<F, R>(app: &App, entry: &Entry, mut post: F) -> Result<Value, Error>
where
    F: FnMut() -> R,
    R: std::future::Future<Output = Result<Value, &'static str>>,
{
    let deadline = tokio::time::Instant::now() + HANDOVER;
    loop {
        let replaced = replaced_outputs(&app.gateway_playbacks, entry);
        if !replaced.is_empty() {
            let settled = futures::future::join_all(replaced.into_iter().map(Output::settled));
            let _ = tokio::time::timeout_at(deadline, settled).await;
            validate_entry(app, entry).await?;
        }
        entry.output.hold();
        match post().await {
            Ok(value) => return Ok(value),
            Err(code) => {
                entry.output.abandon();
                let retry = code == "gateway_capacity"
                    && tokio::time::Instant::now() + RETRY_PAUSE < deadline
                    && !replaced_outputs(&app.gateway_playbacks, entry).is_empty();
                if !retry {
                    return Err(Error::Code(code));
                }
                tracing::info!(session_tag = %crate::gateway::diagnostics::tag(&entry.id), "Gateway capacity still held by the replaced playback; retrying");
                tokio::time::sleep(RETRY_PAUSE).await;
                validate_entry(app, entry).await?;
            }
        }
    }
}
