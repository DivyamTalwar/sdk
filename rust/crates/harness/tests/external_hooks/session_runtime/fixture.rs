use super::*;
use acyclic_fs::{MemoryAuthorityBackend, MemoryObjectBackend};
use acyclic_harness::core::{AuthorityVerifier, Scope};

type Host = FilesystemHost<MemoryAuthorityBackend, MemoryObjectBackend>;
type Runtime = FilesystemTaskRuntime<MemoryStream, MemoryAuthorityBackend, MemoryObjectBackend>;

pub(crate) struct Fixture {
    pub runtime: Runtime,
    pub files: Arc<Host>,
    pub volume: VolumeRef,
    pub verifier: AuthorityVerifier,
    pub signed: Scope,
    pub tasks: TaskRegistry,
    pub revision: String,
    pub publisher: Arc<FilesystemContentPublisher<MemoryAuthorityBackend, MemoryObjectBackend>>,
    pub requests: TaskVolume<MemoryStream>,
    pub results: TaskVolume<MemoryStream>,
}

impl Fixture {
    pub async fn new(binding: VolumeBinding) -> Result<Self> {
    let provider = ProviderRef::new(
        "session-worker".into(),
        "filesystem".into(),
        "2".into(),
        &mut None,
    )?;
    let files = FilesystemHost::new(Fs::memory(), &provider, None)?;
    let agent = AgentId::from_bytes([61; 16]);
    let volume = VolumeRef::new(
        provider,
        "private".into(),
        VolumeClass::AgentPrivate,
        VolumeOwner::Agent(agent),
        &mut None,
    )?;
    files.create_volume(&volume).await?;
    let issuer = AuthorityIssuer::new(
        "session-worker",
        [62; 32],
        Authority {
            kind: AggregateKind::Task,
            id: "session-worker".into(),
        },
    );
    let grants = Capabilities::new([
        "operation:declare".to_owned(),
        "operation:observe".to_owned(),
        "operation:wake".to_owned(),
        "operation:cancel".to_owned(),
        "task:spawn:acyclic.command_session@1".to_owned(),
        "tool:call:write".to_owned(),
        volume.capability(VolumeOperation::Read, &mut None)?,
        volume.capability(VolumeOperation::Write, &mut None)?,
        volume.directory_read_capability("", &mut None)?,
    ]);
    let signed = issuer.issue("owner", &grants, Some(agent), None, &mut None)?;
    let verifier = issuer.verifier(&mut None)?;
    let scope = RuntimeScope::new(grants, Limits::default())?;
    let machine = Arc::new(CommandSessionMachine::new());
    let mut tasks = TaskRegistry::default();
    tasks.register(machine.definition()?)?;
    let mut machines = MachineRegistry::default();
    machines.register(machine, &mut None)?;
    let mut tools = acyclic_harness::tool::ToolRegistry::default();
    let write = acyclic_harness::tool::files::write()?;
    let revision = write.definition.revision.clone();
    tools.register((write, None), &mut None, None)?;
    let stream = Arc::new(MemoryStream::default());
    let requests = TaskVolume::open(
        stream.clone(),
        StreamPath::new("requests").map_err(storage)?,
    )
    .await
    .map_err(storage)?;
    let results = TaskVolume::open(
        if binding == VolumeBinding::Foreign {
            Arc::new(MemoryStream::default())
        } else {
            stream.clone()
        },
        StreamPath::new(if binding == VolumeBinding::Shared {
            "requests"
        } else {
            "results"
        })
        .map_err(storage)?,
    )
    .await
    .map_err(storage)?;
    let runtime = Box::pin(FilesystemTaskRuntime::open(
        stream,
        files.clone(),
        &volume,
        &verifier,
        &signed,
        &scope,
        &tasks,
        &machines,
        &tools,
        SessionLimits {
            active_tasks: 1,
            total_tasks: 1,
            depth: 1,
            model_steps: 1,
        },
        1,
        65_536,
        Arc::new(SystemUnixMillisClock),
        None,
        &[],
        &ResumableToolRegistry::default(),
        None,
    ))
    .await?
    .with_output_volume(&volume, None)?;
    let publisher =
        FilesystemContentPublisher::new(files.clone(), &volume, &verifier, &signed, 65_536, None)?;
        Ok(Self {runtime,files,volume,verifier,signed,tasks,revision,publisher,requests,results})
    }
}
