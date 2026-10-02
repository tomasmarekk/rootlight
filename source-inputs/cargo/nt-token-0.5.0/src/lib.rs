//  _   _ _______     _______          _
// | \ | |_   _\ \   / /_   _|__   ___| |_ ___  _ __ ___
// |  \| | | |  \ \ / /  | |/ _ \ / __| __/ _ \| '__/ _ \
// | |\  | | |   \ V /   | | (_) | (__| || (_) | | |  __/
// |_| \_| |_|    \_/    |_|\___/ \___|\__\___/|_|  \___|
//
//             Windows NT Token Utilities – since 2025
// --------------------------------------------------------
//! # nt_token
//!
//! Memory-safe, ergonomic helpers for working with Windows access tokens and security identifiers (SIDs) in Rust.
//!
//! This crate builds on the `windows` crate and deliberately mirrors the `PathBuf` / `Path` API shape:
//!
//! * `OwnedToken` – owns a `HANDLE` and releases it automatically on `Drop`.
//! * `Token` – zero-cost borrowed view (`#[repr(transparent)]`); most high-level methods live here.
//! * `Sid` – owned, immutable SID with parsing, formatting and account-lookup helpers.
//!
//! ## Quick start
//!
//! ```rust
//! use nt_token::{OwnedToken, Sid};
//! use windows::Win32::Security::TOKEN_QUERY;
//!
//! # fn main() -> windows::core::Result<()> {
//! let token = OwnedToken::from_current_process(TOKEN_QUERY)?;
//! println!("elevated         = {}", token.is_elevated()?);
//! println!("integrity level  = {}", token.integrity_level()?);
//!
//! for g in token.groups()? {
//!     println!("group → {g}");
//! }
//! # Ok(()) }
//! ```

use std::{ffi::c_void, ops::Deref};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, E_INVALIDARG, ERROR_INSUFFICIENT_BUFFER, ERROR_NOT_ALL_ASSIGNED,
            GetLastError, HANDLE, HLOCAL, LUID, LocalFree,
        },
        Security::{
            AdjustTokenGroups, AdjustTokenPrivileges, AllocateAndInitializeSid,
            Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW},
            CREATE_RESTRICTED_TOKEN_FLAGS, CheckTokenMembership, CreateRestrictedToken,
            CreateWellKnownSid, DuplicateTokenEx, FreeSid, GetLengthSid, GetSidSubAuthority,
            GetSidSubAuthorityCount, GetTokenInformation, IsValidSid, IsWellKnownSid,
            LUID_AND_ATTRIBUTES, LookupAccountSidW, LookupPrivilegeNameW, LookupPrivilegeValueW,
            PSID, SE_PRIVILEGE_ENABLED, SE_PRIVILEGE_REMOVED, SECURITY_IMPERSONATION_LEVEL,
            SECURITY_NT_AUTHORITY, SID_IDENTIFIER_AUTHORITY, SID_NAME_USE, TOKEN_ACCESS_MASK,
            TOKEN_APPCONTAINER_INFORMATION, TOKEN_ELEVATION, TOKEN_ELEVATION_TYPE, TOKEN_GROUPS,
            TOKEN_INFORMATION_CLASS, TOKEN_LINKED_TOKEN, TOKEN_MANDATORY_LABEL, TOKEN_PRIVILEGES,
            TOKEN_PRIVILEGES_ATTRIBUTES, TOKEN_TYPE, TOKEN_USER, TokenAppContainerNumber,
            TokenAppContainerSid, TokenCapabilities, TokenDeviceGroups, TokenElevation,
            TokenElevationType, TokenGroups, TokenHasRestrictions, TokenImpersonationLevel,
            TokenIntegrityLevel, TokenIsAppContainer, TokenLinkedToken, TokenLogonSid, TokenOwner,
            TokenPrimary, TokenPrimaryGroup, TokenPrivileges, TokenRestrictedDeviceGroups,
            TokenRestrictedSids, TokenType, TokenUIAccess, TokenUser, TokenVirtualizationAllowed,
            TokenVirtualizationEnabled, WELL_KNOWN_SID_TYPE,
        },
        System::{
            SystemServices::SE_GROUP_ENABLED,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
    core::{BOOL, Error, HSTRING, PCWSTR, PWSTR, Result},
};

fn buffer_probe(res: Result<()>) -> Result<()> {
    match res {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() => Ok(()),
        Err(e) => Err(e),
    }
}

fn nonempty_slice<T>(slice: &[T]) -> Option<&[T]> {
    if slice.is_empty() { None } else { Some(slice) }
}

/* ------------------------------------------------------------------------- */
/* OwnedToken                                                                */
/* ------------------------------------------------------------------------- */

/// RAII owner for a Windows access‑token `HANDLE`.
#[derive(Debug)]
pub struct OwnedToken {
    handle: HANDLE,
}

impl OwnedToken {
    /// Open the current process token with the requested access rights.
    pub fn from_current_process(access: TOKEN_ACCESS_MASK) -> Result<Self> {
        unsafe {
            let mut h = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), access, &mut h)?;
            Ok(Self { handle: h })
        }
    }

    /// Wrap an existing token `HANDLE`.
    ///
    /// # Safety
    /// The caller must ensure that `handle` is a valid access-token handle
    /// that is **exclusively owned** – it will be automatically closed when the
    /// returned `OwnedToken` is dropped.
    pub unsafe fn new(handle: HANDLE) -> Self {
        Self { handle }
    }

    /// Release ownership and return the raw `HANDLE` without closing it.
    ///
    /// This prevents the handle from being closed when the wrapper is dropped;
    /// the caller becomes responsible for eventually calling `CloseHandle`.
    pub fn into_raw(self) -> HANDLE {
        let h = self.handle;
        std::mem::forget(self);
        h
    }
}

impl Drop for OwnedToken {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle).ok() };
    }
}

impl Deref for OwnedToken {
    type Target = Token;
    fn deref(&self) -> &Self::Target {
        unsafe { Token::new(&self.handle) }
    }
}

/* ------------------------------------------------------------------------- */
/* Token                                                                  */
/* ------------------------------------------------------------------------- */

/// Borrowed, zero‑cost view of a token `HANDLE` (like `Path`).
#[repr(transparent)]
#[derive(Copy, Clone, Debug)]
pub struct Token {
    handle: HANDLE,
}

impl Token {
    pub unsafe fn new(handle: &HANDLE) -> &Self {
        unsafe { &*(handle as *const HANDLE as *const Token) }
    }

    /// Raw handle (borrowed).
    pub fn handle(&self) -> HANDLE {
        self.handle
    }

    /// Duplicate the referenced token.
    pub fn duplicate(
        &self,
        access: TOKEN_ACCESS_MASK,
        token_type: TOKEN_TYPE,
        imp_level: SECURITY_IMPERSONATION_LEVEL,
    ) -> Result<OwnedToken> {
        unsafe {
            let mut dup = HANDLE::default();
            DuplicateTokenEx(self.handle, access, None, imp_level, token_type, &mut dup)?;
            Ok(OwnedToken { handle: dup })
        }
    }

    /// Is the token elevated (Vista+ UAC)?
    pub fn is_elevated(&self) -> Result<bool> {
        unsafe {
            let mut elev: TOKEN_ELEVATION = std::mem::zeroed();
            let mut ret = 0u32;
            GetTokenInformation(
                self.handle,
                TokenElevation,
                Some(&mut elev as *mut _ as *mut c_void),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret,
            )?;
            Ok(elev.TokenIsElevated != 0)
        }
    }

    /// Return the integrity-level SID.
    pub fn integrity_level(&self) -> Result<Sid> {
        unsafe {
            let mut len = 0u32;
            buffer_probe(GetTokenInformation(
                self.handle,
                TokenIntegrityLevel,
                None,
                0,
                &mut len,
            ))?;
            let mut buf = vec![0u8; len as usize];
            GetTokenInformation(
                self.handle,
                TokenIntegrityLevel,
                Some(buf.as_mut_ptr() as *mut c_void),
                len,
                &mut len,
            )?;
            let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
            Sid::from_ptr(label.Label.Sid)
        }
    }

    fn groups_of(&self, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<Group>> {
        unsafe {
            let mut len = 0u32;
            buffer_probe(GetTokenInformation(self.handle, class, None, 0, &mut len))?;
            let mut buf = vec![0u8; len as usize];
            GetTokenInformation(
                self.handle,
                class,
                Some(buf.as_mut_ptr() as *mut c_void),
                len,
                &mut len,
            )?;
            let groups = &*(buf.as_ptr() as *const TOKEN_GROUPS);
            let slice =
                std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize);
            slice
                .iter()
                .map(|ga| {
                    Ok(Group {
                        sid: Sid::from_ptr(ga.Sid)?,
                        attributes: ga.Attributes,
                    })
                })
                .collect()
        }
    }

    /// Enumerate group SIDs (`TokenGroups`).
    pub fn groups(&self) -> Result<Vec<Group>> {
        self.groups_of(TokenGroups)
    }

    /// Enumerate capability SIDs (`TokenCapabilities`).
    pub fn capabilities(&self) -> Result<Vec<Group>> {
        self.groups_of(TokenCapabilities)
    }

    /// Enumerate the logon SID (`TokenLogonSid`). Typically returns a single entry.
    pub fn logon_sid(&self) -> Result<Vec<Group>> {
        self.groups_of(TokenLogonSid)
    }

    /// Enumerate restricted SIDs (`TokenRestrictedSids`).
    pub fn restricted_sids(&self) -> Result<Vec<Group>> {
        self.groups_of(TokenRestrictedSids)
    }

    /// Enumerate device group SIDs (`TokenDeviceGroups`).
    pub fn device_groups(&self) -> Result<Vec<Group>> {
        self.groups_of(TokenDeviceGroups)
    }

    /// Enumerate restricted device group SIDs (`TokenRestrictedDeviceGroups`).
    pub fn restricted_device_groups(&self) -> Result<Vec<Group>> {
        self.groups_of(TokenRestrictedDeviceGroups)
    }

    /// For filtered admin tokens, return the linked administrator token.
    pub fn linked_token(&self) -> Result<OwnedToken> {
        unsafe {
            let mut linked: TOKEN_LINKED_TOKEN = std::mem::zeroed();
            let mut ret = 0u32;
            GetTokenInformation(
                self.handle,
                TokenLinkedToken,
                Some(&mut linked as *mut _ as *mut c_void),
                std::mem::size_of::<TOKEN_LINKED_TOKEN>() as u32,
                &mut ret,
            )?;
            Ok(OwnedToken {
                handle: linked.LinkedToken,
            })
        }
    }

    /// Retrieve the token's elevation type (default, limited, or full).
    pub fn elevation_type(&self) -> Result<TOKEN_ELEVATION_TYPE> {
        unsafe {
            let mut et: TOKEN_ELEVATION_TYPE = std::mem::zeroed();
            let mut ret = 0u32;
            GetTokenInformation(
                self.handle,
                TokenElevationType,
                Some(&mut et as *mut _ as *mut c_void),
                std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
                &mut ret,
            )?;
            Ok(et)
        }
    }

    /// Retrieve the impersonation level (only valid for impersonation tokens).
    pub fn impersonation_level(&self) -> Result<SECURITY_IMPERSONATION_LEVEL> {
        unsafe {
            let mut lvl: SECURITY_IMPERSONATION_LEVEL = std::mem::zeroed();
            let mut ret = 0u32;
            GetTokenInformation(
                self.handle,
                TokenImpersonationLevel,
                Some(&mut lvl as *mut _ as *mut c_void),
                std::mem::size_of::<SECURITY_IMPERSONATION_LEVEL>() as u32,
                &mut ret,
            )?;
            Ok(lvl)
        }
    }

    /// Helper: retrieve a variable-length `GetTokenInformation` buffer.
    #[inline]
    fn info_buffer(&self, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<u8>> {
        unsafe {
            let mut len = 0u32;
            buffer_probe(GetTokenInformation(self.handle, class, None, 0, &mut len))?;
            let mut buf = vec![0u8; len as usize];
            GetTokenInformation(
                self.handle,
                class,
                Some(buf.as_mut_ptr() as *mut c_void),
                len,
                &mut len,
            )?;
            Ok(buf)
        }
    }

    /// Helper: retrieve a DWORD token information field.
    #[inline]
    fn dword_info(&self, class: TOKEN_INFORMATION_CLASS) -> Result<u32> {
        unsafe {
            let mut val: u32 = 0;
            let mut ret = 0u32;
            GetTokenInformation(
                self.handle,
                class,
                Some(&mut val as *mut _ as *mut c_void),
                std::mem::size_of::<u32>() as u32,
                &mut ret,
            )?;
            Ok(val)
        }
    }

    /// Helper: retrieve a bool token information field.
    fn bool_info(&self, class: TOKEN_INFORMATION_CLASS) -> Result<bool> {
        Ok(self.dword_info(class)? != 0)
    }

    /// Does the token have any restrictions (sandboxed / filtered token)?
    pub fn has_restrictions(&self) -> Result<bool> {
        self.bool_info(TokenHasRestrictions)
    }

    /// Is process virtualization allowed for this token (UAC file/registry virtualization)?
    pub fn virtualization_allowed(&self) -> Result<bool> {
        self.bool_info(TokenVirtualizationAllowed)
    }

    /// Is process virtualization currently enabled for this token?
    pub fn virtualization_enabled(&self) -> Result<bool> {
        self.bool_info(TokenVirtualizationEnabled)
    }

    /// Is this token an AppContainer token?
    pub fn is_app_container(&self) -> Result<bool> {
        self.bool_info(TokenIsAppContainer)
    }

    /// Is this a primary token (`TokenPrimary`) as opposed to an impersonation token?
    pub fn is_primary(&self) -> Result<bool> {
        unsafe {
            let mut ty: TOKEN_TYPE = std::mem::zeroed();
            let mut ret = 0u32;
            GetTokenInformation(
                self.handle,
                TokenType,
                Some(&mut ty as *mut _ as *mut c_void),
                std::mem::size_of::<TOKEN_TYPE>() as u32,
                &mut ret,
            )?;
            Ok(ty == TokenPrimary)
        }
    }

    /// Return the token's primary user SID.
    pub fn user(&self) -> Result<Sid> {
        let buf = self.info_buffer(TokenUser)?;
        let tu = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
        unsafe { Sid::from_ptr(tu.User.Sid) }
    }

    /// Return the token's primary group SID.
    pub fn primary_group(&self) -> Result<Sid> {
        let buf = self.info_buffer(TokenPrimaryGroup)?;
        let tpg =
            unsafe { &*(buf.as_ptr() as *const windows::Win32::Security::TOKEN_PRIMARY_GROUP) };
        unsafe { Sid::from_ptr(tpg.PrimaryGroup) }
    }

    /// Return the token's owner SID.
    pub fn owner(&self) -> Result<Sid> {
        let buf = self.info_buffer(TokenOwner)?;
        let to = unsafe { &*(buf.as_ptr() as *const windows::Win32::Security::TOKEN_OWNER) };
        unsafe { Sid::from_ptr(to.Owner) }
    }

    /// Enumerate privileges contained in the token.
    pub fn privileges(&self) -> Result<Vec<Privilege>> {
        unsafe {
            let mut len = 0u32;
            buffer_probe(GetTokenInformation(
                self.handle,
                TokenPrivileges,
                None,
                0,
                &mut len,
            ))?;

            let mut buf = vec![0u8; len as usize];
            GetTokenInformation(
                self.handle,
                TokenPrivileges,
                Some(buf.as_mut_ptr() as *mut c_void),
                len,
                &mut len,
            )?;

            let tprivs = &*(buf.as_ptr() as *const TOKEN_PRIVILEGES);
            let slice = std::slice::from_raw_parts(
                tprivs.Privileges.as_ptr(),
                tprivs.PrivilegeCount as usize,
            );

            Ok(slice.iter().map(|la| Privilege::from_raw(la)).collect())
        }
    }

    /// Adjust multiple privileges in one go. The token must have
    /// `TOKEN_ADJUST_PRIVILEGES` access. Each `Privilege` decides whether
    /// it should be enabled (`Privilege::new(name, true)`) or disabled
    /// (`Privilege::new(name, false)`).
    pub fn adjust_privileges(&self, privs: &[Privilege]) -> Result<()> {
        if privs.is_empty() {
            return Ok(());
        }

        unsafe {
            // Allocate a variable-length TOKEN_PRIVILEGES buffer.
            let count = privs.len();
            let buf_len = std::mem::size_of::<TOKEN_PRIVILEGES>()
                + (count - 1) * std::mem::size_of::<LUID_AND_ATTRIBUTES>();
            let mut buf = vec![0u8; buf_len];

            // Write PrivilegeCount.
            *(buf.as_mut_ptr() as *mut u32) = count as u32;

            // Pointer to the first LUID_AND_ATTRIBUTES entry.
            let la_ptr =
                buf.as_mut_ptr().add(std::mem::size_of::<u32>()) as *mut LUID_AND_ATTRIBUTES;

            for (i, p) in privs.iter().enumerate() {
                *la_ptr.add(i) = LUID_AND_ATTRIBUTES {
                    Luid: p.inner.Luid,
                    Attributes: if p.is_enabled() {
                        SE_PRIVILEGE_ENABLED
                    } else {
                        SE_PRIVILEGE_REMOVED
                    },
                };
            }

            let tp_ptr = buf.as_ptr() as *const TOKEN_PRIVILEGES;

            AdjustTokenPrivileges(self.handle, false, Some(&*tp_ptr), 0, None, None)?;
            if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
                return Err(Error::from_win32());
            }
        }
        Ok(())
    }

    /// Check whether this token contains the specified SID (group membership).
    pub fn check_membership(&self, sid: &Sid) -> Result<bool> {
        unsafe {
            let mut is_member = BOOL(0);
            // Safety: `sid.buf` is a valid SID buffer.
            CheckTokenMembership(
                Some(self.handle),
                PSID(sid.buf.as_ptr() as *mut c_void),
                &mut is_member,
            )?;
            Ok(is_member.as_bool())
        }
    }

    /// Does the token have UIAccess privilege (non-elevated UI automation)?
    pub fn ui_access(&self) -> Result<bool> {
        self.bool_info(TokenUIAccess)
    }

    /// Return the app-container SID associated with this token, or `None` if the token is not an AppContainer.
    pub fn app_container_sid(&self) -> Result<Option<Sid>> {
        let buf = self.info_buffer(TokenAppContainerSid)?;
        let info = unsafe { &*(buf.as_ptr() as *const TOKEN_APPCONTAINER_INFORMATION) };
        if info.TokenAppContainer.0.is_null() {
            return Ok(None);
        }
        let sid = unsafe { Sid::from_ptr(info.TokenAppContainer)? };
        Ok(Some(sid))
    }

    /// Return the app-container number assigned by the system.
    pub fn app_container_number(&self) -> Result<u32> {
        self.dword_info(TokenAppContainerNumber)
    }

    /// Adjust multiple group states in one go. The token must have
    /// `TOKEN_ADJUST_GROUPS` access. Each `Group` instance specifies the
    /// desired attribute flags for an existing SID in the token. For
    /// example, to enable a group, include the `SE_GROUP_ENABLED` attribute;
    /// to disable it, omit that flag. Analogous to `adjust_privileges`.
    pub fn adjust_groups(&self, groups: &[Group]) -> Result<()> {
        if groups.is_empty() {
            return Ok(());
        }

        unsafe {
            let sid_attrs = Self::sid_attr_vec(groups);
            let count = sid_attrs.len();

            // Compute total size for TOKEN_GROUPS buffer.
            let buf_len = std::mem::size_of::<TOKEN_GROUPS>()
                + (count - 1) * std::mem::size_of::<windows::Win32::Security::SID_AND_ATTRIBUTES>();

            let usize_slots =
                (buf_len + std::mem::size_of::<usize>() - 1) / std::mem::size_of::<usize>();
            let mut buf: Vec<usize> = vec![0; usize_slots];

            let tg_ptr = buf.as_mut_ptr() as *mut TOKEN_GROUPS;

            (*tg_ptr).GroupCount = count as u32;

            let sa_ptr = (*tg_ptr).Groups.as_mut_ptr();
            std::ptr::copy_nonoverlapping(sid_attrs.as_ptr(), sa_ptr, count);

            AdjustTokenGroups(self.handle, false, Some(&*tg_ptr), 0, None, None)?;

            if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
                return Err(Error::from_win32());
            }
        }

        Ok(())
    }

    /// Internal helper: build a Vec<SID_AND_ATTRIBUTES> pointing at the SIDs
    /// owned by the provided `Group` slice. The returned vector keeps the
    /// pointers valid for the lifetime of the vector.
    fn sid_attr_vec(groups: &[Group]) -> Vec<windows::Win32::Security::SID_AND_ATTRIBUTES> {
        groups
            .iter()
            .map(|g| windows::Win32::Security::SID_AND_ATTRIBUTES {
                Sid: PSID(g.sid.buf.as_ptr() as *mut _),
                Attributes: g.attributes(),
            })
            .collect()
    }

    /// Create a restricted token derived from this token.
    /// `flags` is a bitmask of `CREATE_RESTRICTED_TOKEN_*` constants (e.g.
    /// `DISABLE_MAX_PRIVILEGE`). The various slices correspond to the
    /// parameters of the Win32 `CreateRestrictedToken` API.
    pub fn create_restricted_token(
        &self,
        flags: CREATE_RESTRICTED_TOKEN_FLAGS,
        sids_to_disable: &[Group],
        privileges_to_delete: &[Privilege],
        sids_to_restrict: &[Group],
    ) -> Result<OwnedToken> {
        unsafe {
            // Build SID_AND_ATTRIBUTES buffers.
            let disable_vec = Self::sid_attr_vec(sids_to_disable);
            let restrict_vec = Self::sid_attr_vec(sids_to_restrict);
            // Build LUID_AND_ATTRIBUTES array for privileges.
            let priv_vec: Vec<LUID_AND_ATTRIBUTES> =
                privileges_to_delete.iter().map(|p| p.inner).collect();
            let mut new_tok = HANDLE::default();

            CreateRestrictedToken(
                self.handle,
                flags,
                nonempty_slice(&disable_vec),
                nonempty_slice(&priv_vec),
                nonempty_slice(&restrict_vec),
                &mut new_tok,
            )?;

            Ok(OwnedToken { handle: new_tok })
        }
    }
}

impl<'a> From<&'a OwnedToken> for Token {
    fn from(tok: &'a OwnedToken) -> Self {
        Token { handle: tok.handle }
    }
}

/* ------------------------------------------------------------------------- */
/* Sid                                                                       */
/* ------------------------------------------------------------------------- */

/// Owned and immutable security identifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Sid {
    buf: Vec<u8>,
}

impl Sid {
    /// Parse canonical string form ("S‑1‑…").
    pub fn parse(s: &str) -> Result<Self> {
        unsafe {
            let mut psid = PSID::default();
            let hstring = HSTRING::from(s);
            ConvertStringSidToSidW(PCWSTR(hstring.as_ptr()), &mut psid)?;
            let len = GetLengthSid(psid);
            let slice = std::slice::from_raw_parts(psid.0 as *const u8, len as usize);
            let v = slice.to_vec();
            LocalFree(Some(HLOCAL(psid.0 as *mut c_void)));
            Ok(Self { buf: v })
        }
    }

    /// Canonical string representation.
    pub fn to_string(&self) -> Result<String> {
        unsafe {
            let mut pwstr = PWSTR::null();
            ConvertSidToStringSidW(PSID(self.buf.as_ptr() as *mut c_void), &mut pwstr)?;
            let s = pwstr.to_string();
            LocalFree(Some(HLOCAL(pwstr.0 as *mut c_void)));
            Ok(s?)
        }
    }

    /// Resolve account & domain names.
    pub fn account(&self) -> Result<(String, String)> {
        unsafe {
            let mut name_len = 0u32;
            let mut dom_len = 0u32;
            let mut use_ty = SID_NAME_USE(0);
            buffer_probe(LookupAccountSidW(
                PCWSTR::null(),
                PSID(self.buf.as_ptr() as *mut _),
                None,
                &mut name_len,
                None,
                &mut dom_len,
                &mut use_ty,
            ))?;
            let mut name = vec![0u16; name_len as usize];
            let mut dom = vec![0u16; dom_len as usize];
            LookupAccountSidW(
                PCWSTR::null(),
                PSID(self.buf.as_ptr() as *mut _),
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                Some(PWSTR(dom.as_mut_ptr())),
                &mut dom_len,
                &mut use_ty,
            )?;
            Ok((
                String::from_utf16(&name[..name_len as usize])?,
                String::from_utf16(&dom[..dom_len as usize])?,
            ))
        }
    }

    /// Build a Windows well‑known SID.
    pub fn well_known(kind: WELL_KNOWN_SID_TYPE) -> Result<Self> {
        unsafe {
            let mut size = 0u32;
            buffer_probe(CreateWellKnownSid(kind, None, None, &mut size))?;
            let mut buf = vec![0u8; size as usize];
            CreateWellKnownSid(
                kind,
                None,
                Some(PSID(buf.as_mut_ptr() as *mut _)),
                &mut size,
            )?;
            Ok(Self { buf })
        }
    }

    /* -------------- helpers --------------------------------------------- */
    pub(crate) unsafe fn from_ptr(psid: PSID) -> Result<Self> {
        unsafe {
            let len = GetLengthSid(psid);
            if len == 0 {
                return Err(Error::from_win32());
            }
            let slice = std::slice::from_raw_parts(psid.0 as *const u8, len as usize);
            Ok(Self {
                buf: slice.to_vec(),
            })
        }
    }

    /// Last sub‑authority (RID).
    pub fn rid(&self) -> Result<u32> {
        unsafe {
            let cnt_ptr = GetSidSubAuthorityCount(PSID(self.buf.as_ptr() as *mut c_void));
            if cnt_ptr.is_null() {
                return Err(Error::from_win32());
            }
            let rid_ptr = GetSidSubAuthority(
                PSID(self.buf.as_ptr() as *mut c_void),
                (*cnt_ptr as u32) - 1,
            );
            if rid_ptr.is_null() {
                return Err(Error::from_win32());
            }
            Ok(*rid_ptr)
        }
    }

    /// Is this SID structurally valid (according to `IsValidSid`)?
    pub fn is_valid(&self) -> bool {
        unsafe { IsValidSid(PSID(self.buf.as_ptr() as *mut _)).as_bool() }
    }

    /// Does this SID match the specified well-known SID type?
    pub fn is_well_known(&self, kind: WELL_KNOWN_SID_TYPE) -> bool {
        unsafe { IsWellKnownSid(PSID(self.buf.as_ptr() as *mut _), kind).as_bool() }
    }

    /// Build a SID from an identifier authority and a slice of 32-bit
    /// sub-authorities. Internally this uses `AllocateAndInitializeSid`, which
    /// supports up to 8 sub-authorities.
    pub fn from_parts(authority: &SID_IDENTIFIER_AUTHORITY, subids: &[u32]) -> Result<Self> {
        if subids.len() > 8 {
            return Err(Error::new(E_INVALIDARG, "too many RIDs"));
        }

        unsafe {
            let mut psid = PSID::default();
            // Zero-fill the parameters for the varargs call.
            let mut subs = [0u32; 8];
            for (i, &rid) in subids.iter().enumerate() {
                subs[i] = rid;
            }

            AllocateAndInitializeSid(
                authority,
                subids.len() as u8,
                subs[0],
                subs[1],
                subs[2],
                subs[3],
                subs[4],
                subs[5],
                subs[6],
                subs[7],
                &mut psid,
            )?;

            // Copy into a Rust-managed buffer then free the Win32 allocation.
            let len = GetLengthSid(psid);
            let slice = std::slice::from_raw_parts(psid.0 as *const u8, len as usize);
            let buf = slice.to_vec();
            FreeSid(psid);

            Ok(Self { buf })
        }
    }

    /// Convenience helper: construct an `S-1-5-…` SID under the NT authority
    /// (`SECURITY_NT_AUTHORITY`) from the provided subauthorities.
    pub fn from_nt_authority(subids: &[u32]) -> Result<Self> {
        Self::from_parts(&SECURITY_NT_AUTHORITY, subids)
    }
}

impl std::fmt::Display for Sid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.to_string() {
            Ok(s) => write!(f, "{s}"),
            Err(_) => write!(f, "<invalid sid>"),
        }
    }
}

/* ------------------------------------------------------------------------- */
/* Group                                                                     */
/* ------------------------------------------------------------------------- */

/// Token group entry – SID plus its attribute flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    sid: Sid,
    attributes: u32,
}

impl Group {
    /// Borrow the underlying SID.
    pub fn sid(&self) -> &Sid {
        &self.sid
    }

    /// Raw attribute flags (see `SE_GROUP_*`).
    pub fn attributes(&self) -> u32 {
        self.attributes
    }

    /// Convenience helper – resolve account/domain names (delegates to the SID).
    pub fn account(&self) -> Result<(String, String)> {
        self.sid.account()
    }

    /// Construct a group specification from a `Sid` and an explicit
    /// attribute mask (see `SE_GROUP_*`). This gives you full control over the
    /// `SID_AND_ATTRIBUTES::Attributes` field that will be passed to
    /// `AdjustTokenGroups`.
    pub fn new(sid: Sid, attributes: u32) -> Self {
        Self { sid, attributes }
    }

    /// Convenience constructor: create an enabled group specification
    /// (`SE_GROUP_ENABLED`).
    pub fn enabled(sid: Sid) -> Self {
        Self::new(sid, SE_GROUP_ENABLED as u32)
    }

    /// Convenience constructor: create a disabled group specification
    /// (no attributes set).
    pub fn disabled(sid: Sid) -> Self {
        Self::new(sid, 0)
    }
}

impl std::fmt::Display for Group {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Delegate to the SID's `Display` impl for concise output.
        write!(f, "{}", self.sid)
    }
}

/* ------------------------------------------------------------------------- */
/* Privilege                                                                 */
/* ------------------------------------------------------------------------- */

/// Token privilege (immutable snapshot).
#[derive(Clone, Debug)]
pub struct Privilege {
    inner: LUID_AND_ATTRIBUTES,
}

impl Privilege {
    /// Internal helper – build from a raw `LUID_AND_ATTRIBUTES` entry.
    pub(crate) fn from_raw(inner: &LUID_AND_ATTRIBUTES) -> Self {
        Self { inner: *inner }
    }

    /// Return the privilege name (e.g. `SeDebugPrivilege`).
    pub fn name(&self) -> Result<String> {
        unsafe {
            let mut len = 0u32;
            buffer_probe(LookupPrivilegeNameW(
                PCWSTR::null(),
                &self.inner.Luid,
                None,
                &mut len,
            ))?;
            let mut buf = vec![0u16; len as usize];
            LookupPrivilegeNameW(
                PCWSTR::null(),
                &self.inner.Luid,
                Some(PWSTR(buf.as_mut_ptr())),
                &mut len,
            )?;
            Ok(String::from_utf16(&buf[..len as usize])?)
        }
    }

    /// Raw attributes bitmask (see `SE_PRIVILEGE_*` constants).
    pub fn attributes(&self) -> TOKEN_PRIVILEGES_ATTRIBUTES {
        self.inner.Attributes
    }

    /// Is this privilege currently enabled?
    pub fn is_enabled(&self) -> bool {
        self.inner.Attributes.contains(SE_PRIVILEGE_ENABLED)
    }

    /// Construct a privilege specification by name and desired enabled state.
    /// `name` is the textual privilege name (e.g. `"SeDebugPrivilege"`).
    pub fn new(name: &str, enable: bool) -> Result<Self> {
        unsafe {
            // Resolve the privilege's LUID.
            let mut luid = LUID::default();
            let hstring = HSTRING::from(name);
            LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(hstring.as_ptr()), &mut luid)?;

            Ok(Self {
                inner: LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: if enable {
                        SE_PRIVILEGE_ENABLED
                    } else {
                        SE_PRIVILEGE_REMOVED
                    },
                },
            })
        }
    }

    /// Convenience constructor: create an enabled privilege specification
    /// (equivalent to `Privilege::new(name, true)`).
    pub fn enabled(name: &str) -> Result<Self> {
        Self::new(name, true)
    }

    /// Convenience constructor: create a disabled privilege specification
    /// (equivalent to `Privilege::new(name, false)`).
    pub fn disabled(name: &str) -> Result<Self> {
        Self::new(name, false)
    }
}

impl std::fmt::Display for Privilege {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.name().unwrap_or_else(|_| {
            let luid = self.inner.Luid;
            format!("LUID({:?},{:?})", luid.HighPart, luid.LowPart)
        });
        let state = if self.is_enabled() {
            "enabled"
        } else {
            "disabled"
        };
        write!(f, "{name} ({state})")
    }
}
