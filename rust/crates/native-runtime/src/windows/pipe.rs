//! Current-user pipe permissions and authenticated server identity.
use std::{ffi::c_void, io, os::windows::io::AsRawHandle, ptr};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_KERNEL_OBJECT,
        },
        DACL_SECURITY_INFORMATION, GetAce, GetLengthSid, GetSecurityDescriptorControl,
        GetTokenInformation, OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
        TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::{
        Pipes::GetNamedPipeServerProcessId,
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

struct LocalMemory(*mut c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        // SAFETY: this owns the result of a successful LocalAlloc-backed conversion.
        unsafe { LocalFree(self.0) };
    }
}

/// Immutable pipe permissions bound to the current process's token user.
pub struct UserPipeSecurity {
    descriptor: LocalMemory,
    user: Vec<u8>,
}

// SAFETY: the descriptor is immutable after construction; the kernel only reads it.
unsafe impl Send for UserPipeSecurity {}
// SAFETY: shared callers only read the immutable descriptor and user SID.
unsafe impl Sync for UserPipeSecurity {}

impl std::fmt::Debug for UserPipeSecurity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserPipeSecurity").finish_non_exhaustive()
    }
}

impl UserPipeSecurity {
    /// Builds a protected DACL granting access only to this token's user SID.
    pub fn current_user() -> io::Result<Self> {
        // SAFETY: this pseudo-handle is valid and must not be closed.
        let token = TokenUserBuffer::read(unsafe { GetCurrentProcess() })?;
        let mut text = ptr::null_mut();
        // SAFETY: the SID lies inside the live token buffer; text is an output.
        if unsafe { ConvertSidToStringSidW(token.sid(), &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let text_memory = LocalMemory(text.cast());
        let mut length = 0;
        // SAFETY: the conversion returned a NUL-terminated UTF-16 allocation.
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: length was measured within the allocation above.
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
            .map_err(io::Error::other)?;
        drop(text_memory);
        let sddl: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})\0")
            .encode_utf16()
            .collect();
        let mut descriptor = ptr::null_mut();
        // SAFETY: sddl is live and NUL-terminated; descriptor is an output.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor: LocalMemory(descriptor),
            user: token.bytes().to_vec(),
        })
    }

    /// Creates an instance with the protected DACL, including replacement instances.
    pub fn create(&self, name: &str, first: bool) -> io::Result<NamedPipeServer> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
                .map_err(io::Error::other)?,
            lpSecurityDescriptor: self.descriptor.0,
            bInheritHandle: 0,
        };
        // SAFETY: the attributes and immutable descriptor outlive pipe creation.
        let server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    name,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                )
        }?;
        self.validate_access(&server)?;
        Ok(server)
    }

    /// Rejects a connected server belonging to another user before any RPC is sent.
    pub fn authenticate(&self, client: &NamedPipeClient) -> io::Result<()> {
        authenticate_server(client, &self.user)
    }

    fn validate_access(&self, server: &NamedPipeServer) -> io::Result<()> {
        let mut descriptor = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        let mut owner = ptr::null_mut();
        // SAFETY: server is live and all output pointers are writable.
        let status = unsafe {
            GetSecurityInfo(
                server.as_raw_handle(),
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(
                i32::try_from(status).map_err(io::Error::other)?,
            ));
        }
        let _descriptor = LocalMemory(descriptor);
        let mut control = 0;
        let mut revision = 0;
        // SAFETY: GetSecurityInfo initialized this descriptor.
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let denied = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "pipe is not restricted to the current user",
            )
        };
        if owner.is_null() || dacl.is_null() || control & SE_DACL_PROTECTED == 0 {
            return Err(denied());
        }
        // SAFETY: owner and dacl belong to the live kernel-provided descriptor.
        if unsafe { sid_bytes(owner) } != self.user || unsafe { (*dacl).AceCount } != 1 {
            return Err(denied());
        }
        let mut ace = ptr::null_mut();
        // SAFETY: the ACL contains one ACE; ace is a writable output.
        if unsafe { GetAce(dacl, 0, &mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
        // SAFETY: the ACE header is valid; only access-allowed ACEs have this SID layout.
        if unsafe { (*ace).Header.AceType } != 0 || unsafe { (*ace).Header.AceFlags } != 0 {
            return Err(denied());
        }
        // SAFETY: the access-allowed ACE and its SID are inside the descriptor.
        if unsafe { sid_bytes(ptr::addr_of_mut!((*ace).SidStart).cast()) } != self.user {
            return Err(denied());
        }
        Ok(())
    }
}

unsafe fn sid_bytes<'a>(sid: *mut c_void) -> &'a [u8] {
    // SAFETY: callers retain the valid kernel-provided SID for the returned borrow.
    unsafe { std::slice::from_raw_parts(sid.cast(), GetLengthSid(sid) as usize) }
}

fn authenticate_server(client: &NamedPipeClient, expected: &[u8]) -> io::Result<()> {
    let mut server = 0;
    // SAFETY: client owns a live pipe handle; server is writable.
    if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut server) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: opening a process ID has no preconditions; failure returns null.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, server) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let user = TokenUserBuffer::read(process);
    // SAFETY: process is an owned handle returned above, closed exactly once.
    unsafe { CloseHandle(process) };
    if user?.bytes() != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe server belongs to another user",
        ));
    }
    Ok(())
}

struct TokenUserBuffer(Vec<u64>);
impl TokenUserBuffer {
    fn read(process: HANDLE) -> io::Result<Self> {
        let mut token = ptr::null_mut();
        // SAFETY: process is a live process handle and token is writable.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let mut length = 0;
            // SAFETY: a zero-sized buffer requests only the required length.
            unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut length) };
            let mut buffer = vec![
                0_u64;
                usize::try_from(length)
                    .map_err(io::Error::other)?
                    .div_ceil(8)
                    .max(1)
            ];
            // SAFETY: the buffer holds at least length writable, 8-aligned bytes.
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    length,
                    &mut length,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(buffer))
        })();
        // SAFETY: token is an owned handle returned above, closed exactly once.
        unsafe { CloseHandle(token) };
        result
    }

    fn sid(&self) -> *mut c_void {
        // SAFETY: GetTokenInformation initialized TOKEN_USER in this aligned buffer.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
    fn bytes(&self) -> &[u8] {
        // SAFETY: the SID points within the live token buffer initialized by Windows.
        unsafe { std::slice::from_raw_parts(self.sid().cast(), GetLengthSid(self.sid()) as usize) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE,
        Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT},
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
        SE_DACL_PROTECTED,
    };

    #[tokio::test]
    async fn every_instance_has_only_the_current_user_in_a_protected_dacl() -> io::Result<()> {
        let security = UserPipeSecurity::current_user()?;
        let directory = tempfile::tempdir()?;
        let name = format!(
            r"\\.\pipe\acyclic-security-{}",
            directory
                .path()
                .file_name()
                .ok_or_else(|| io::Error::other("temporary directory has no name"))?
                .to_string_lossy()
        );
        let first = security.create(&name, true)?;
        let second = security.create(&name, false)?;
        // SAFETY: this pseudo-handle is valid and must not be closed.
        let user = TokenUserBuffer::read(unsafe { GetCurrentProcess() })?;
        for pipe in [&first, &second] {
            let mut descriptor = ptr::null_mut();
            let mut dacl = ptr::null_mut();
            // SAFETY: pipe is live; the output pointers are writable.
            let status = unsafe {
                GetSecurityInfo(
                    pipe.as_raw_handle(),
                    SE_KERNEL_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut dacl,
                    ptr::null_mut(),
                    &mut descriptor,
                )
            };
            assert_eq!(status, 0);
            let _descriptor = LocalMemory(descriptor);
            let mut control = 0;
            let mut revision = 0;
            // SAFETY: GetSecurityInfo returned this live descriptor.
            assert_ne!(
                unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
                0
            );
            assert_ne!(control & SE_DACL_PROTECTED, 0);
            assert!(!dacl.is_null());
            // SAFETY: the kernel returned a valid, non-null ACL.
            assert_eq!(unsafe { (*dacl).AceCount }, 1);
            let mut ace = ptr::null_mut();
            // SAFETY: the ACL has exactly one ACE and ace is an output.
            assert_ne!(unsafe { GetAce(dacl, 0, &mut ace) }, 0);
            let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
            // SAFETY: our descriptor contains one access-allowed ACE with a SID.
            unsafe {
                assert_eq!((*ace).Header.AceType, 0);
                assert_ne!(
                    EqualSid(ptr::addr_of_mut!((*ace).SidStart).cast(), user.sid()),
                    0
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn authenticates_the_server_and_rejects_a_different_expected_user() -> io::Result<()> {
        let security = UserPipeSecurity::current_user()?;
        let directory = tempfile::tempdir()?;
        let name = format!(
            r"\\.\pipe\acyclic-identity-{}",
            directory
                .path()
                .file_name()
                .ok_or_else(|| io::Error::other("temporary directory has no name"))?
                .to_string_lossy()
        );
        let server = security.create(&name, true)?;
        let client = tokio::net::windows::named_pipe::ClientOptions::new().open(&name)?;
        server.connect().await?;
        security.authenticate(&client)?;
        // S-1-5-7 is Anonymous, distinct from the interactive test user's SID.
        let anonymous = [1, 1, 0, 0, 0, 0, 0, 5, 7, 0, 0, 0];
        let mut other_user = UserPipeSecurity::current_user()?;
        other_user.user = anonymous.to_vec();
        assert_eq!(
            other_user
                .authenticate(&client)
                .err()
                .map(|error| error.kind()),
            Some(io::ErrorKind::PermissionDenied)
        );
        Ok(())
    }
    #[tokio::test]
    async fn rundown_cannot_join_an_unprotected_pipe_namespace() -> io::Result<()> {
        let directory = tempfile::tempdir()?;
        let name = format!(
            r"\\.\pipe\acyclic-unprotected-{}",
            directory
                .path()
                .file_name()
                .ok_or_else(|| io::Error::other("temporary directory has no name"))?
                .to_string_lossy()
        );
        let _unprotected = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)?;
        let security = UserPipeSecurity::current_user()?;
        assert_eq!(
            security
                .create(&name, false)
                .err()
                .map(|error| error.kind()),
            Some(io::ErrorKind::PermissionDenied)
        );
        Ok(())
    }
}
