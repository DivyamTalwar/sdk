//! Replay the real run state machine over in-process filesystem/stream providers.
use super::*;
use crate::{
    context::CompactionPolicy,
    resources::ProviderRef,
    test_support::ScriptedTurn,
    tool::{JsonToolProjection, ToolDefinition},
};
use acyclic_fs::{EmbeddedCapabilities, Fs, MemoryObjectBackend, StreamAuthorityStore};
use acyclic_stream::UnixMillisClock;
use std::sync::Arc;

mod model;
mod schedule;
mod stream;
use model::{Effect, ModelEffects};
use schedule::{Exit, Schedule, check_cut, drive};

type AuthorityBackend = StreamAuthorityStore<stream::Stream>;
type Harness = LocalHarness<stream::Stream, AuthorityBackend, MemoryObjectBackend>;
const PROMPT: &str = "recover the original run input";

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Text,
    EffectAndContinuation,
}

struct Clock;
impl UnixMillisClock for Clock {
    fn now_unix_millis(&self) -> u64 {
        1
    }
}

struct Fixture {
    objects: MemoryObjectBackend,
    stream: Arc<stream::Stream>,
    issuer: AuthorityIssuer,
    volumes: [VolumeRef; 4],
    provider: Arc<ModelEffects>,
    effect: Arc<Effect>,
    scenario: Scenario,
}

impl Fixture {
    async fn new(scenario: Scenario) -> Result<Self> {
        let schedule = Arc::new(Schedule::default());
        let stream = Arc::new(stream::Stream {
            memory: acyclic_stream::MemoryStream::new_with_clock(
                Default::default(),
                Arc::new(Clock),
            ),
            schedule: schedule.clone(),
        });
        let (objects, bucket) = acyclic_objects::v1::MemoryObjects::with_default_bucket();
        let objects = acyclic_fs::LogicalObjectStore::new(Arc::new(objects), bucket);
        let fs = Fs::new(
            StreamAuthorityStore::new(stream.clone()),
            acyclic_fs::LogicalObjectStore::new(
                objects.provider().clone(),
                objects.bucket().clone(),
            ),
            EmbeddedCapabilities::MEMORY,
        );
        let reference =
            ProviderRef::new("local".into(), "filesystem".into(), "2".into(), &mut None)?;
        let files = FilesystemHost::new(fs, &reference, None)?;
        let agent = AgentId::from_bytes([1; 16]);
        let volume = |name| {
            VolumeRef::new(
                reference.clone(),
                name,
                VolumeClass::AgentPrivate,
                VolumeOwner::Agent(agent),
                &mut None,
            )
        };
        let volumes = [
            volume("project".into())?,
            volume("scratch".into())?,
            volume("journal".into())?,
            volume("tasks".into())?,
        ];
        for volume in &volumes {
            files.create_volume(volume).await?;
        }
        let turns = match scenario {
            Scenario::Text => vec![ScriptedTurn::Text("done".into())],
            Scenario::EffectAndContinuation => vec![
                model::tool_turn(),
                ScriptedTurn::Text("root".into()),
                ScriptedTurn::Text("done".into()),
            ],
        };
        Ok(Self {
            objects,
            stream,
            issuer: AuthorityIssuer::new(
                "acyclic.local",
                [7; 32],
                Authority {
                    kind: AggregateKind::Task,
                    id: TaskId::from_bytes([2; 16]).to_string(),
                },
            ),
            volumes,
            provider: Arc::new(ModelEffects::new(turns, schedule.clone())),
            effect: Arc::new(Effect {
                schedule,
                calls: Default::default(),
            }),
            scenario,
        })
    }

    async fn open(&self) -> Result<Harness> {
        // Reconstruct the host and discard all filesystem/runtime caches. Only
        // the two provider states and the external effect receipts survive.
        let fs = Fs::new(
            StreamAuthorityStore::new(self.stream.clone()),
            acyclic_fs::LogicalObjectStore::new(
                self.objects.provider().clone(),
                self.objects.bucket().clone(),
            ),
            EmbeddedCapabilities::MEMORY,
        );
        let files = FilesystemHost::new(fs, self.volumes[0].provider(), None)?;
        let verifier = self.issuer.verifier(&mut None)?;
        let mut tools = ToolRegistry::default();
        if matches!(self.scenario, Scenario::EffectAndContinuation) {
            tools.register(
                (
                    Tool {
                        hook_names: &[],
                        definition: ToolDefinition {
                            name: "test.effect".into(),
                            revision: "1".into(),
                            description: "Increment an external counter".into(),
                            input_schema: serde_json::json!({"type":"object"}),
                            output_schema: serde_json::json!({"type":"object"}),
                            projection_schema: crate::tool::json_projection_schema(
                                serde_json::json!({"type":"object"}),
                            ),
                        },
                        executor: self.effect.clone(),
                        projection: Arc::new(JsonToolProjection),
                    },
                    None,
                ),
                &mut None,
                None,
            )?;
        }
        let (runtime, signed) = runtime::open(runtime::Configuration {
            files: files.clone(),
            stream: self.stream.clone(),
            task_volume: &self.volumes[3],
            issuer: &self.issuer,
            verifier: &verifier,
            agent: AgentId::from_bytes([1; 16]),
            project: &self.volumes[0],
            scratch: &self.volumes[1],
            journal: &self.volumes[2],
            tools: &tools,
            #[cfg(any(feature = "mcp-stdio", feature = "mcp-http"))]
            capabilities: &Capabilities::default(),
            limits: Limits::default(),
            run_limits: TaskRunLimits::default(),
            session_limits: SessionLimits {
                active_tasks: 2,
                total_tasks: 2,
                depth: 1,
                model_steps: 4,
            },
            concurrency: 1,
            clock: Arc::new(Clock),
        })
        .await?;
        // The continuation's ordinary GOAL.md path must be reachable through
        // its selected writer. This scenario selects the private goal volume.
        let runtime = match self.scenario {
            Scenario::EffectAndContinuation => {
                runtime.with_output_volume(&self.volumes[1], None)?
            }
            Scenario::Text => runtime,
        };
        let task_files = Arc::new(
            crate::task_volume::TaskFiles::open(
                self.stream.clone(),
                &self.volumes[3],
                &verifier,
                &signed,
                Limits::default().file_bytes,
            )
            .await?,
        );
        Ok(LocalHarness {
            task_files,
            files: files.clone(),
            runtime: Arc::new(runtime),
            project: self.volumes[0].clone(),
            scratch: self.volumes[1].clone(),
            journal: self.volumes[2].clone(),
            verifier,
            signed,
            model: Model::new(
                "scripted".into(),
                "test".into(),
                "1".into(),
                serde_json::Value::Null,
            )?,
            provider: self.provider.clone(),
            context: ContextPipeline::default(),
            limits: Limits::default(),
            compaction: CompactionPolicy::Disabled,
            #[cfg(any(feature = "mcp-stdio", feature = "mcp-http"))]
            mcp_diagnostics: vec![],
            goal_continuations: matches!(self.scenario, Scenario::EffectAndContinuation).then_some(
                crate::goal::GoalContinuationPolicy {
                    max_continuations: 1,
                },
            ),
            admission: StateArc::new(Mutex::new(())),
            #[cfg(feature = "native-execution")]
            workspace: None,
        })
    }

    async fn initialize(&self, harness: &Harness) -> Result<()> {
        if matches!(self.scenario, Scenario::EffectAndContinuation) {
            FilesystemContentPublisher::new(
                harness.files.clone(),
                &harness.scratch,
                &harness.verifier,
                &harness.signed,
                harness.limits.file_bytes,
                None,
            )?
            .stage_once(
                OperationId::from_bytes([8; 16]),
                "GOAL.md",
                b"finish the objective",
                "text/markdown",
                "GOAL.md",
                None,
            )?
            .await?;
        }
        Ok(())
    }

    fn assert_effects(&self) {
        let (models, tools) = match self.scenario {
            Scenario::Text => (1, 0),
            Scenario::EffectAndContinuation => (3, 1),
        };
        assert_eq!(
            self.provider.accepted(),
            models,
            "missing or duplicated model effect"
        );
        assert_eq!(
            self.effect.calls.lock().unwrap().len(),
            tools,
            "missing or duplicated tool effect"
        );
    }
}

struct Coverage {
    points: Vec<&'static str>,
    unadmitted: bool,
}

async fn case(
    scenario: Scenario,
    cut: Option<usize>,
    baseline: &[&'static str],
) -> Result<Coverage> {
    let fixture = Fixture::new(scenario).await?;
    let result: Result<Coverage> = Box::pin(async {
        let harness = fixture.open().await?;
        fixture.initialize(&harness).await?;
        let schedule = &fixture.stream.schedule;
        schedule.start();
        let (exit, hit) = drive(harness.run(PROMPT), cut, schedule);
        let points = schedule.points();
        assert_eq!(points.len(), hit, "uncontrolled I/O changed the schedule");
        if let Some(cut) = cut {
            check_cut(&exit, cut, hit).expect("every cancellation must fire");
            assert_eq!(
                points,
                baseline[..cut],
                "replay diverged before cancellation"
            );
        } else {
            let Exit::Completed(output) = exit else {
                panic!("baseline cancelled")
            };
            assert_eq!(output?.text, "done");
        }
        drop(harness);
        if cut.is_some() {
            // All operations are owned by this one future: dropping it drains every
            // in-process call. The executor has no spawn API or detached work queue.
            let trace = schedule.drained();
            assert_eq!(
                schedule.drained(),
                trace,
                "publication or dispatch after cancel"
            );
        }
        schedule.recover();
        let reopened = fixture.open().await?;
        let current = reopened.current_turn().await?.0;
        let mut unadmitted = false;
        if let Some(current) = &current {
            let task = crate::runtime::task_id_for_admission(current.operation);
            if matches!(
                reopened.runtime.task_host().outcome(task).await,
                Err(Error::NotFound(_))
            ) {
                unadmitted = true;
                // Negative control for the pre-#672 implementation: direct outcome
                // and attach reject this actual accepted HEAD/admission cut.
                let definition = reopened
                    .runtime
                    .harness()
                    .task::<serde_json::Value, crate::executor::TurnOutput>(
                        "acyclic.stock_turn@1",
                    )?;
                assert!(matches!(
                    reopened.runtime.harness().attach(task, &definition).await,
                    Err(Error::NotFound(_))
                ));
                assert!(reopened.head_is_terminal(Some(current)).await?);
            }
        }
        let (root, output) = if let Some(current) = current {
            let root = crate::runtime::task_id_for_admission(current.root);
            (root, reopened.resume(root).await?)
        } else {
            let output = reopened.run(PROMPT).await?;
            (
                crate::runtime::task_id_for_admission(
                    reopened.current_turn().await?.0.unwrap().root,
                ),
                output,
            )
        };
        assert_eq!(output.text, "done");
        fixture.assert_effects();
        for _ in 0..2 {
            assert_eq!(reopened.resume(root).await?.text, "done");
            fixture.assert_effects();
        }
        drop(reopened);
        let reopened = fixture.open().await?;
        assert_eq!(reopened.resume(root).await?.text, "done");
        fixture.assert_effects();
        Ok(Coverage { points, unadmitted })
    })
    .await;
    result.map_err(|error| Error::Storage(format!("{scenario:?} cut {cut:?}, in-flight boundary {:?}, accepted model effects {} and tool effects {}: {error:?}", fixture.stream.schedule.failure(), fixture.provider.accepted(), fixture.effect.calls.lock().unwrap().len())))
}

fn sweep(scenarios: &[Scenario]) -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let started = std::time::Instant::now();
    // Enter a frozen timer driver without a Tokio task or its cooperative poll
    // budget. Only the explicit provider schedule can suspend these futures.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .map_err(storage)?;
    let _entered = runtime.enter();
    let mut schedules = Vec::new();
    for &scenario in scenarios {
        let baseline = futures::executor::block_on(Box::pin(case(scenario, None, &[])))?.points;
        assert!(!baseline.is_empty());
        eprintln!(
            "{scenario:?}: {} deterministic cancellation points",
            baseline.len()
        );
        schedules.push((scenario, baseline));
    }
    // Each case owns its single-thread executor and independent providers. Run
    // the complete replay matrix in parallel; no filesystem or I/O workers.
    let cuts: Vec<_> = schedules
        .iter()
        .flat_map(|(scenario, baseline)| {
            (1..=baseline.len()).map(move |cut| (*scenario, baseline, cut))
        })
        .collect();
    let next = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let unadmitted = AtomicUsize::new(0);
    let handle = tokio::runtime::Handle::current();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..std::thread::available_parallelism()
            .unwrap()
            .get()
            .min(cuts.len()))
            .map(|_| {
                scope.spawn(|| -> Result<()> {
                    let _entered = handle.enter();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(scenario, baseline, cut)) = cuts.get(index) else {
                            return Ok(());
                        };
                        let coverage = futures::executor::block_on(Box::pin(case(
                            scenario,
                            Some(cut),
                            baseline,
                        )))
                        .map_err(|error| {
                            Error::Storage(format!(
                                "{scenario:?} cut {cut}/{}: {error:?}",
                                baseline.len()
                            ))
                        })?;
                        unadmitted.fetch_add(usize::from(coverage.unadmitted), Ordering::Relaxed);
                        completed.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap()?;
        }
        Ok::<_, Error>(())
    })?;
    assert_eq!(
        completed.load(Ordering::Relaxed),
        cuts.len(),
        "a replay was skipped"
    );
    assert!(
        unadmitted.load(Ordering::Relaxed) > 0,
        "HEAD/admission crash window was not covered"
    );
    eprintln!("complete replay sweep: {:?}", started.elapsed());
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    Ok(())
}

#[test]
fn run_cancellation_at_every_poll_recovers_head_and_admission() -> Result<()> {
    sweep(&[Scenario::Text, Scenario::EffectAndContinuation])
}

#[test]
fn skipped_cancellation_point_is_rejected() {
    let schedule = Schedule::default();
    schedule.start();
    let (complete, _) = drive(
        async {
            schedule.point("first").await;
            schedule.point("second").await;
        },
        None,
        &schedule,
    );
    assert!(matches!(complete, Exit::Completed(())));
    // Deliberately skip the second point in the replay. Completion is a failure,
    // even though the shortened future returned successfully.
    let (exit, hit) = drive(
        async {
            schedule.point("first").await;
        },
        Some(2),
        &schedule,
    );
    assert!(check_cut(&exit, 2, hit).is_err());
}
