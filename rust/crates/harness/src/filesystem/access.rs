use super::*;
use acyclic_fs::access::{
    Action, Capability, Context, InvalidPolicy, PathTrie, Policy, Principal, PrincipalKind,
    Resource,
};
use acyclic_fs::model::{CheckoutMode, GenerationSelector};
use acyclic_stream::UnixMillisClock as _;
use std::marker::PhantomData;

// Explicit initial policy: signed capabilities supply the authority ceiling.
// Reads never synthesize this record when it is missing from an existing volume.
pub(in crate::filesystem) const INITIAL_POLICY: &[u8] = include_bytes!("access/initial.json");
const LEGACY_READ: &[u8] = include_bytes!("access/legacy-read.json");

fn record<'a, T: PreparationRecord>(
    io: &'a PreparationIo,
    bytes: &[u8],
    code: crate::wire::ErrorCode,
) -> Result<(T, DecodeReservation<'a>)> {
    if bytes.len() as u64 > acyclic_fs::access::MAXIMUM_POLICY_RECORD_BYTES {
        return Err(stock_boundary(
            Some(io),
            code,
            "filesystem access record exceeds bounds",
        ));
    }
    io.decode(PhantomData::<T>, bytes, code)
}

#[cfg(any(test, feature = "test-support"))]
mod verification;
#[cfg(feature = "test-support")]
pub use verification::check_access_record;

impl<A: AsyncAuthorityStore, O: AsyncObjectStore> FilesystemHost<A, O> {
    pub(in crate::filesystem) async fn read_file_with_access(
        &self,
        scope: &Scope,
        reference: &FileRef,
        maximum_bytes: u64,
        inherited_reference: bool,
        io: &PreparationIo,
    ) -> Result<acyclic_stream::OwnedBytes> {
        reference.validate(&mut Some(&mut io.history_io()))?;
        if reference.volume().provider() != &self.provider
            || maximum_bytes == 0
            || reference.descriptor().byte_length() > maximum_bytes
        {
            return Err(io.invalid("content provider or size limit mismatch"));
        }
        let label = crate::core::filesystem_scope_label(scope, &mut Some(&mut io.history_io()))?;
        // A legacy grant is authority for this exact request only. A structured
        // payload is always its ceiling, even when legacy labels are also present.
        if label.is_none() && !inherited_reference {
            super::require_file_read(scope, reference, io)?;
        }
        let payload = if let Some(label) = label {
            label
                .strip_prefix(acyclic_fs::access::SCOPE_PREFIX)
                .ok_or_else(|| io.invalid("filesystem capability label is invalid"))?
                .as_bytes()
        } else {
            LEGACY_READ
        };
        let capability = record::<Capability>(io, payload, crate::wire::ErrorCode::Unauthorized)?;
        let workspace_ref =
            fork_preparer_io::content_workspace(&self.provider, reference.volume(), Some(io))?;
        let workspace_hold = io.guard_owned(workspace_ref.logical_owned_bytes()?)?;
        let workspace_ref = (workspace_ref, workspace_hold);
        let workspace = self.open(&workspace_ref.0, Some(io)).await?;
        let head = io.sdk(
            workspace
                .checkout(
                    GenerationSelector::Head,
                    CheckoutMode::read_only_pinned(),
                    io,
                    io.cancellation(),
                )
                .await,
        )?;
        let head_hold = if io.original_native_work().is_none() {
            Some(io.reserve_decode(head.logical_owned_bytes())?)
        } else {
            None
        };
        let mut head = (head, head_hold);
        let bytes = io
            .sdk(head.0.read_access_policy(io, io.cancellation()).await)?
            .ok_or_else(|| {
                stock_boundary(
                    Some(io),
                    crate::wire::ErrorCode::Unauthorized,
                    "volume has no access policy",
                )
            })?;
        if io.original_native_work().is_none() {
            io.retain_owned(bytes.logical_owned_bytes)?;
        }
        let bytes_hold = io.guard_bytes(&bytes.bytes)?;
        let bytes = (bytes, bytes_hold);
        let policy = record::<Policy>(io, &bytes.0.bytes, crate::wire::ErrorCode::Storage)?;
        let mut generation = [0; 32];
        io.charge(WorkCounters {
            source_bytes_read: reference.version().len() as u64,
            items_examined: reference.version().len() as u64,
            ..Default::default()
        })?;
        hex::decode_to_slice(reference.version(), &mut generation)
            .map_err(|_| io.invalid("file version is not a Filesystem generation"))?;
        let (selected, latest) = io.sdk(
            head.0
                .generation_position(
                    GenerationId::new(Digest::from_bytes(generation)),
                    io,
                    io.cancellation(),
                )
                .await,
        )?;
        let subject = crate::executor::with_history_io(Some(io), |work| match scope.agent() {
            Some(agent) => crate::executor::preparation_format(format_args!("{agent}"), work),
            None => crate::executor::preparation_format(format_args!("{}", scope.issuer()), work),
        })?;
        let subject_hold = io.guard_owned(subject.capacity() as u64)?;
        let principal = (
            Principal {
                kind: if scope.agent().is_some() {
                    PrincipalKind::Agent
                } else {
                    PrincipalKind::Service
                },
                id: subject,
                lineage: vec![],
                groups: Default::default(),
                roles: Default::default(),
                tags: Default::default(),
                attributes: Default::default(),
            },
            subject_hold,
        );
        let volume = crate::executor::with_history_io(Some(io), |work| {
            crate::executor::preparation_format(
                format_args!(
                    "{}",
                    uuid::Uuid::from_bytes(head.0.volume_id().into_bytes())
                ),
                work,
            )
        })?;
        let volume_hold = io.guard_owned(volume.capacity() as u64)?;
        let volume = (volume, volume_hold);
        let context = Context {
            now: acyclic_stream::SystemUnixMillisClock.now_unix_millis() / 1000,
            approvals: 0,
        };
        let resource = Resource {
            config: head.0.volume_config(),
            lineage: &[],
            volume: &volume.0,
            generation: selected.get(),
            head: latest.get(),
            path: reference.path(),
        };
        let mut remaining = usize::try_from(io.remaining()?.items_examined).unwrap_or(usize::MAX);
        let before = remaining;
        let mut holds = [None, None];
        let mut stage = 0;
        let mut admission_error = None;
        let compiled = PathTrie::compile_with_work(
            &policy.0,
            &principal.0,
            resource,
            context,
            &capability.0,
            &mut remaining,
            |bytes| match io.reserve_decode(bytes as u64) {
                Ok(hold) => {
                    *holds
                        .get_mut(stage)
                        .ok_or(InvalidPolicy("invalid compilation stage"))? = Some(hold);
                    stage += 1;
                    Ok(())
                }
                Err(error) => {
                    admission_error = Some(error);
                    Err(InvalidPolicy("compilation storage was not admitted"))
                }
            },
        );
        io.charge(WorkCounters {
            items_examined: (before - remaining) as u64,
            ..Default::default()
        })?;
        if let Some(error) = admission_error {
            return Err(error);
        }
        let mut compiled = (
            compiled.map_err(|_| {
                stock_boundary(
                    Some(io),
                    crate::wire::ErrorCode::Unauthorized,
                    "filesystem policy compilation refused read",
                )
            })?,
            holds,
        );
        let mut retained = compiled.0.logical_owned_bytes();
        for hold in compiled.1.iter_mut().flatten() {
            let bytes = retained.min(hold.bytes());
            hold.substitute(bytes)?;
            retained -= bytes;
        }
        if retained != 0 {
            return Err(io.invalid("compiled access storage exceeds admission"));
        }
        let allowed = compiled
            .0
            .decide(Action::Read, reference.path(), context.now)
            .allowed;
        drop(compiled);
        drop(volume);
        drop(principal);
        drop(policy);
        drop(capability);
        drop(bytes);
        drop(head);
        if !allowed {
            return Err(stock_boundary(
                Some(io),
                crate::wire::ErrorCode::Unauthorized,
                "filesystem access decision denied read",
            ));
        }
        let pinned = io.sdk(
            workspace
                .generation(
                    GenerationId::new(Digest::from_bytes(generation)),
                    io,
                    io.cancellation(),
                )
                .await,
        )?;
        fork_preparer_io::retain(&pinned, Some(io)).await?;
        let path = crate::executor::with_history_io(Some(io), |work| {
            crate::executor::preparation_format(format_args!("/{}", reference.path()), work)
        })?;
        let path_hold = io.guard_owned(path.capacity() as u64)?;
        let path = (path, path_hold);
        let bytes = read_generation(&pinned, &path.0, maximum_bytes, Some(io)).await?;
        reference
            .descriptor()
            .verify(&bytes, &mut Some(&mut io.history_io()))?;
        Ok(bytes)
    }
}
