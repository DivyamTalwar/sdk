use super::*;
use crate::Capabilities;
use crate::core::{AggregateKind, AuthorityIssuer};

#[tokio::test]
async fn local_content_publication_futures_stay_bounded() -> Result<()> {
    let directory = tempfile::tempdir().map_err(|error| Error::Storage(error.to_string()))?;
    let fs = Fs::local(acyclic_fs::LocalOptions::new(directory.path()))
        .await
        .map_err(|error| Error::Storage(error.to_string()))?;
    let provider = ProviderRef::new("sizes".into(), "filesystem".into(), "2".into(), &mut None)?;
    let host = FilesystemHost::new(fs, &provider, None)?;
    let owner = AgentId::from_bytes([32; 16]);
    let volume = VolumeRef::new(
        provider,
        "private".into(),
        VolumeClass::AgentPrivate,
        VolumeOwner::Agent(owner),
        &mut None,
    )?;
    let issuer = AuthorityIssuer::new(
        "sizes",
        [31; 32],
        Authority {
            kind: AggregateKind::Conversation,
            id: "owner".into(),
        },
    );
    let scope = issuer.issue(
        "write",
        &Capabilities::new([volume.capability(VolumeOperation::Write, &mut None)?]),
        Some(owner),
        None,
        &mut None,
    )?;
    let grant = ContentGrant::verify(
        &issuer.verifier(&mut None)?,
        &scope,
        &volume,
        VolumeOperation::Write,
        4096,
        &mut None,
    )?;
    host.create_volume(&volume).await?;
    let key = IdempotencyKey::new("sizes")?;
    let publication = host.put_content(
        &volume,
        &grant,
        "file.txt",
        b"content",
        "text/plain",
        "file.txt",
        4096,
        &key,
        None,
        None,
    );
    let bytes = std::mem::size_of_val(&publication);
    eprintln!("local content publication future: {bytes} bytes");
    assert!(bytes < 4096, "publication future is {bytes} bytes");
    let file = publication.await?;
    assert_eq!(
        host.read_pinned_io(&file, 4096, None).await?.as_ref(),
        b"content"
    );
    Ok(())
}
