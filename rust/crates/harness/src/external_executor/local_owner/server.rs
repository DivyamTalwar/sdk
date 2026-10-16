//! The surviving local owner serves the SDK Streams API, never harness RPCs.
use super::*;
use crate::task_volume::{Component, FileKey, FileValue, Query, TaskFile, TaskVolume};
use acyclic_stream::grpc::Service as StreamService;
use fs2::FileExt as _;
use futures::StreamExt as _;
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tonic::{
    Status,
    body::Body,
    codegen::{Service, http},
    server::NamedService,
    transport::{Identity, Server, ServerTlsConfig},
};

const CONTROL: &str = "owner/control";

#[derive(Clone)]
struct Owner {
    registry: Arc<registry::Registry>,
    control: Arc<LocalStream>,
    secret: [u8; 32],
}

#[derive(Clone)]
struct Follows(Owner);

impl NamedService for Follows {
    const NAME: &'static str =
        <acyclic_stream::wire::stream_session_service_server::StreamSessionServiceServer<
            acyclic_stream::session::Service<LocalStream>,
        > as NamedService>::NAME;
}

impl Service<http::Request<Body>> for Follows {
    type Response = <Owner as Service<http::Request<Body>>>::Response;
    type Error = Infallible;
    type Future = <Owner as Service<http::Request<Body>>>::Future;
    fn poll_ready(
        &mut self,
        context: &mut Context<'_>,
    ) -> Poll<std::result::Result<(), Infallible>> {
        self.0.poll_ready(context)
    }
    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        self.0.call(request)
    }
}

impl NamedService for Owner {
    const NAME: &'static str = <acyclic_stream::wire::stream_service_server::StreamServiceServer<
        StreamService<LocalStream>,
    > as NamedService>::NAME;
}

impl Service<http::Request<Body>> for Owner {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<std::result::Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let owner = self.clone();
        let key = request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or_else(|| Error::Unauthorized("local Streams credential is absent".into()))
            .and_then(|token| credential::verify(&owner.secret, token));
        Box::pin(async move {
            let selected: Result<_> = async {
                let key = key?;
                if key == "owner" {
                    return Ok(provider::ClientStream::control(Arc::clone(&owner.control)));
                }
                let harness = owner.registry.get(&key).await?;
                Ok(provider::ClientStream::workspace(harness.task_stream()))
            }
            .await;
            let provider = match selected {
                Ok(provider) => provider,
                Err(error) => return Ok(Status::permission_denied(error.to_string()).into_http()),
            };
            if request.uri().path().split('/').nth(1) == Some(Follows::NAME) {
                let mut service = acyclic_stream::wire::stream_session_service_server::StreamSessionServiceServer::new(
                    acyclic_stream::session::Service::new(Arc::new(provider), "local".into(), Default::default()),
                );
                service.call(request).await
            } else {
                let mut service =
                    acyclic_stream::wire::stream_service_server::StreamServiceServer::new(
                        StreamService::new(Arc::new(provider)),
                    );
                service.call(request).await
            }
        })
    }
}

fn stop_key(instance: uuid::Uuid) -> Result<FileKey> {
    Ok(FileKey::Task {
        task: Component::new("owner").map_err(storage)?,
        file: TaskFile::Request(Component::new(instance.to_string()).map_err(storage)?),
    })
}

async fn stopped(control: &TaskVolume<LocalStream>, instance: uuid::Uuid) -> Result<()> {
    let mut observation = control
        .observe(Query::File(stop_key(instance)?), None, 1)
        .await
        .map_err(storage)?;
    if observation
        .snapshot
        .is_some_and(|snapshot| !snapshot.files.is_empty())
    {
        return Ok(());
    }
    observation
        .changes
        .next()
        .await
        .ok_or_else(|| Error::Storage("owner stop follow ended".into()))?
        .map_err(storage)?;
    Ok(())
}

/// Runs the existing local ownership boundary until an authenticated stop file.
pub fn run_local_owner() -> impl Future<Output = Result<()>> {
    Box::pin(serve())
}

async fn serve() -> Result<()> {
    let ready = acyclic_native_runtime::take_service_ready_signal().map_err(storage)?;
    let root = directory()?;
    let lock = bootstrap::lock(&root, "instance.lock")?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
        Err(error) => return Err(storage(error)),
    }
    let control = Arc::new(
        LocalStream::open(root.join("stream"), Default::default())
            .await
            .map_err(storage)?,
    );
    let files = Arc::new(
        TaskVolume::open(
            Arc::clone(&control),
            StreamPath::new(CONTROL).map_err(storage)?,
        )
        .await
        .map_err(storage)?,
    );
    let secret = bootstrap::secret(&root)?;
    let instance = uuid::Uuid::new_v4();
    let registry = Arc::new(registry::Registry::new(
        root.clone(),
        Arc::clone(&files),
        instance,
    ));
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(storage)?;
    let certificate =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .map_err(storage)?;
    let pem = certificate.cert.pem();
    let address = Address {
        endpoint: format!("https://{}", listener.local_addr().map_err(storage)?),
        certificate: pem.clone(),
        instance,
    };
    bootstrap::publish(
        &root.join("address.json"),
        &serde_json::to_vec(&address).map_err(storage)?,
        true,
    )?;
    let owner = Owner {
        registry: Arc::clone(&registry),
        control,
        secret,
    };
    let server = Server::builder()
        .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(
            pem,
            certificate.signing_key.serialize_pem(),
        )))
        .map_err(storage)?
        .add_service(owner.clone())
        .add_service(Follows(owner));
    let incoming = futures::stream::try_unfold(listener, |listener| async move {
        let (socket, _) = listener.accept().await?;
        Ok::<_, std::io::Error>(Some((socket, listener)))
    });
    if let Some(ready) = ready {
        let _ = ready.signal();
    }
    let shutdown = Arc::clone(&files);
    let result = server
        .serve_with_incoming_shutdown(Box::pin(incoming), async move {
            let _ = stopped(&shutdown, instance).await;
        })
        .await;
    let drained = registry.drain().await;
    drop(registry);
    drop(files);
    drop(lock);
    result.map_err(storage)?;
    drained
}

/// Requests shutdown through the same SDK file publication path and awaits drain.
pub async fn stop_local_owner() -> Result<()> {
    let root = directory()?;
    let address = Address::from_bytes(&read(&root.join("address.json"))?)?;
    let key = bootstrap::secret(&root)?;
    let client = Arc::new(connect(&address, &credential::credential(&key, "owner")).await?);
    let files = TaskVolume::open_existing(client, StreamPath::new(CONTROL).map_err(storage)?)
        .await
        .map_err(storage)?;
    let file = FileValue::new(stop_key(address.instance)?, b"{}".as_slice());
    super::super::publish_request(&files, &file, None, None)
        .await
        .map_err(storage)?;
    drop(files);
    let lock = bootstrap::lock(&root, "instance.lock")?;
    tokio::task::spawn_blocking(move || lock.lock_exclusive().map_err(storage))
        .await
        .map_err(storage)??;
    Ok(())
}

pub(super) async fn drive(
    harness: Arc<LocalHarness>,
    stop: Arc<TaskVolume<LocalStream>>,
    instance: uuid::Uuid,
) -> Result<()> {
    let (requests, _) = harness.external_volumes();
    let mut observation = requests
        .observe(Query::Root, None, acyclic_stream::MAX_ITEMS as usize)
        .await
        .map_err(storage)?;
    let initial = futures::stream::iter(
        observation
            .snapshot
            .take()
            .map(|snapshot| snapshot.files)
            .unwrap_or_default()
            .into_iter()
            .map(Ok),
    );
    let followed = observation.changes.filter_map(|change| async move {
        match change {
            Ok(change) => change.file.map(Ok),
            Err(error) => Some(Err(error)),
        }
    });
    let files = initial.chain(followed);
    tokio::pin!(files);
    let stopping = stopped(&stop, instance);
    tokio::pin!(stopping);
    loop {
        let file = tokio::select! {
            biased;
            result = &mut stopping => return result,
            file = files.next() => match file {
                Some(file) => file.map_err(storage)?,
                None => return Err(Error::Storage("local request follow ended".into())),
            }
        };
        let FileKey::Task {
            task,
            file: TaskFile::Request(_),
        } = &file.key
        else {
            continue;
        };
        let request = super::super::ExternalRequest::from_file(task, &file).map_err(storage)?;
        harness.external_request(&request).await?;
    }
}
