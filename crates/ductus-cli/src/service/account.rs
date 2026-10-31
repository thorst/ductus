//! `ductus service account`: which account the service runs as, and changing it.
//!
//! The installer sets the account (`SERVICEACCOUNT`, default `NT SERVICE\Ductus`) but can't take
//! a password: installer properties end up in the install log. A gMSA has none (Windows keeps it),
//! so it needs nothing more; an account with a password is given it here, after an install and
//! after every upgrade (which reinstalls the service). Changing the account also grants it "Log on
//! as a service", makes sure the service's own SID is in its token (the data folder's permissions
//! name `NT SERVICE\Ductus`, whatever account runs it), and restarts the service if it was running.
//!
//! `ductus service installed` is the installer's step after it installs the service: the same
//! right and SID type, and an Event Log warning when the account still needs its password.

use super::eventlog::{self, Kind as Event};
use super::messages as id;
use super::{NAME, data_dir};
use anyhow::{Context, Result, bail};
use ductus_core::service_account::Kind;
use std::ffi::OsStr;
use std::time::{Duration, Instant};
use windows_service::service::{ServiceAccess, ServiceSidType, ServiceState};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_LOGON_FAILURE, ERROR_LOGON_TYPE_NOT_GRANTED, HANDLE};
use windows_sys::Win32::Security::Authentication::Identity::{
    LSA_HANDLE, LSA_OBJECT_ATTRIBUTES, LSA_UNICODE_STRING, LsaAddAccountRights, LsaClose, LsaEnumerateAccountRights,
    LsaFreeMemory, LsaNtStatusToWinError, LsaOpenPolicy, POLICY_CREATE_ACCOUNT, POLICY_LOOKUP_NAMES,
};
use windows_sys::Win32::Security::{LOGON32_LOGON_SERVICE, LOGON32_PROVIDER_DEFAULT, LogonUserW, LookupAccountNameW};
use windows_sys::Win32::System::Services::{ChangeServiceConfigW, SERVICE_NO_CHANGE};

/// The right a service's account needs, unless it's one of the computer's own.
const LOGON_RIGHT: &str = "SeServiceLogonRight";
/// Group Policy's "Deny log on as a service", which wins over the right.
const DENY_RIGHT: &str = "SeDenyServiceLogonRight";
/// How long a restart may take: the clean stop finishes the messages in hand first.
const WAIT: Duration = Duration::from_secs(180);

/// The installed service's account, or None when there's no service.
pub fn current() -> Result<Option<String>> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("connecting to the service manager")?;
    let service = match manager.open_service(NAME, ServiceAccess::QUERY_CONFIG) {
        Ok(s) => s,
        Err(windows_service::Error::Winapi(e)) if e.raw_os_error() == Some(1060) => return Ok(None),
        Err(e) => return Err(e).context("opening the Ductus service"),
    };
    let config = service.query_config().context("reading the service's settings")?;
    Ok(Some(config.account_name.map_or_else(|| "LocalSystem".into(), |a| a.to_string_lossy().into_owned())))
}

/// `ductus service account` with no name: who the service runs as, and what that means.
pub fn show() -> Result<()> {
    let Some(account) = current()? else {
        bail!("there is no Ductus service on this computer (the installer adds it)");
    };
    println!("the Ductus service runs as {account}");
    match Kind::of(&account) {
        Kind::Computer => println!(
            "  one of this computer's own accounts: on the network (a SQL Server's Windows login) it is {}",
            network_name().unwrap_or_else(|| "nobody (this computer is in no domain)".into())
        ),
        Kind::Managed => println!("  a group managed service account (gMSA): Windows keeps its password"),
        Kind::Password => println!(
            "  an account with a password: give it again after every upgrade (`ductus service account {account} --password-stdin`)"
        ),
    }
    if Kind::of(&account) != Kind::Computer {
        let sid = sid_of(&account)?;
        let rights = rights_of(&sid).unwrap_or_default();
        if rights.iter().any(|r| r == DENY_RIGHT) {
            println!(
                "  ✗ denied \"Log on as a service\" (by Group Policy, most likely): the service can't start as it"
            );
        } else if rights.iter().any(|r| r == LOGON_RIGHT) {
            println!("  ✓ may log on as a service");
        } else {
            println!(
                "  ⚠ not given \"Log on as a service\" itself (a group it's in may have it); `ductus service account {account}` grants it"
            );
        }
    }
    Ok(())
}

/// `ductus service account NAME`: run the service as NAME.
pub fn change(name: &str, password_stdin: bool, start: bool) -> Result<()> {
    let name = name.trim();
    let kind = Kind::of(name);
    if kind != Kind::Computer && !name.contains('\\') && !name.contains('@') {
        bail!(
            r"write the account as DOMAIN\name (a gMSA ends in $: DOMAIN\svc-ductus$), or .\name for one on this computer"
        );
    }
    if password_stdin && !kind.needs_password() {
        bail!("{name} has no password to give (Windows keeps a gMSA's; the computer's own accounts have none)");
    }
    let password = kind.needs_password().then(|| read_password(name, password_stdin)).transpose()?;

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("connecting to the service manager (run this as an administrator)")?;
    let access = ServiceAccess::QUERY_CONFIG
        | ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::QUERY_STATUS
        | ServiceAccess::START
        | ServiceAccess::STOP;
    let service = manager
        .open_service(NAME, access)
        .context("opening the Ductus service (is it installed? run this as an administrator)")?;
    let before = current()?.unwrap_or_default();

    if kind != Kind::Computer {
        let sid = sid_of(name)?;
        if rights_of(&sid).unwrap_or_default().iter().any(|r| r == DENY_RIGHT) {
            bail!(
                "{name} is denied \"Log on as a service\" (Group Policy's \"Deny log on as a service\"): the service couldn't start as it; ask whoever manages the policy"
            );
        }
        grant_logon_right(&sid).with_context(|| format!("granting {name} \"Log on as a service\""))?;
        println!("✓ {name} may log on as a service");
    }
    if let Some(password) = &password {
        check_password(name, password)?;
        println!("✓ the password is right (a service logon as {name} works)");
    }
    service
        .set_config_service_sid_info(ServiceSidType::Unrestricted)
        .context("giving the service its own SID (for the data folder's permissions)")?;
    set_account(service.raw_handle(), name, password.as_deref())?;
    println!("✓ the service runs as {name} from its next start");
    println!(
        "  it reads {} through the service's own SID (NT SERVICE\\Ductus), whatever the account",
        data_dir().display()
    );
    eventlog::write(
        Event::Information,
        id::ACCOUNT_CHANGED,
        &[&format!("from: {before}\nto: {name}\nby: {}", super::who())],
    );

    let running = service.query_status()?.current_state != ServiceState::Stopped;
    if running {
        println!("restarting it (the clean stop finishes the messages in hand first)…");
        service.stop().context("stopping the service")?;
        wait_for(&service, ServiceState::Stopped)?;
    }
    if running || start {
        service.start(&[] as &[&OsStr]).map_err(|e| started_error(e, name))?;
        match wait_for(&service, ServiceState::Running) {
            Ok(()) => println!("✓ the service is running as {name}"),
            Err(e) => bail!(
                "{e:#}: the account is set, but the service didn't reach Running; see the Event Log (Windows Logs → Application, source Ductus) and {}",
                super::run::log_dir().display()
            ),
        }
    } else {
        println!("the service is stopped: start it with `Start-Service Ductus` (or add --start)");
    }
    Ok(())
}

/// `ductus service installed`: the installer's step after it installs the service.
pub fn installed() -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("connecting to the service manager")?;
    let service = manager
        .open_service(NAME, ServiceAccess::QUERY_CONFIG | ServiceAccess::CHANGE_CONFIG)
        .context("opening the Ductus service")?;
    service.set_config_service_sid_info(ServiceSidType::Unrestricted).context("giving the service its own SID")?;
    let account = current()?.unwrap_or_default();
    let kind = Kind::of(&account);
    if kind != Kind::Computer {
        // Where Group Policy owns the right, the next refresh may take this away again (docs/nuances/windows.md).
        if let Err(e) = sid_of(&account).and_then(|sid| grant_logon_right(&sid)) {
            eventlog::write(
                Event::Warning,
                id::ERROR,
                &[&format!("The installer couldn't grant {account} \"Log on as a service\": {e:#}")],
            );
        }
    }
    if kind.needs_password() {
        eventlog::write(Event::Warning, id::NEEDS_PASSWORD, &[&account]);
    }
    Ok(())
}

fn read_password(name: &str, from_stdin: bool) -> Result<String> {
    use std::io::BufRead;
    let password = if from_stdin {
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).context("reading the password from stdin")?;
        line.trim_end_matches(['\r', '\n']).to_string()
    } else {
        rpassword::prompt_password(format!("{name}'s password: ")).context("reading the password")?
    };
    if password.is_empty() {
        bail!("{name} needs its password (or name a gMSA, which has none)");
    }
    Ok(password)
}

/// A service logon with the password: proves the password and the right before anything changes.
fn check_password(name: &str, password: &str) -> Result<()> {
    let (domain, user) = match name.split_once('\\') {
        Some((d, u)) => (Some(d), u),
        None => (None, name), // user@domain
    };
    let (domain, user, password) = (domain.map(wide), wide(user), wide(password));
    let mut token: HANDLE = std::ptr::null_mut();
    let ok = unsafe {
        LogonUserW(
            user.as_ptr(),
            domain.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
            password.as_ptr(),
            LOGON32_LOGON_SERVICE,
            LOGON32_PROVIDER_DEFAULT,
            &mut token,
        )
    };
    if ok != 0 {
        unsafe { CloseHandle(token) };
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error().map(|c| c as u32) {
        Some(ERROR_LOGON_FAILURE) => bail!("Windows refused {name}'s password (a wrong password, or no such account)"),
        Some(ERROR_LOGON_TYPE_NOT_GRANTED) => bail!(
            "{name} may not log on as a service even after the grant: Group Policy most likely denies it (\"Deny log on as a service\") or manages the right; ask whoever manages the policy to add it"
        ),
        _ => Err(e).with_context(|| format!("checking {name}'s password")),
    }
}

fn set_account(
    handle: windows_sys::Win32::System::Services::SC_HANDLE,
    name: &str,
    password: Option<&str>,
) -> Result<()> {
    let account = wide(name);
    // The computer's own accounts and gMSAs take an empty password.
    let password = wide(password.unwrap_or(""));
    let ok = unsafe {
        ChangeServiceConfigW(
            handle,
            SERVICE_NO_CHANGE,
            SERVICE_NO_CHANGE,
            SERVICE_NO_CHANGE,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            account.as_ptr(),
            password.as_ptr(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("setting the service's account to {name}"));
    }
    Ok(())
}

fn started_error(e: windows_service::Error, name: &str) -> anyhow::Error {
    match &e {
        windows_service::Error::Winapi(io) if io.raw_os_error() == Some(1069) => anyhow::anyhow!(
            "the service couldn't log on as {name} (Windows error 1069): the password, or the right to log on as a service (Group Policy can take it away)"
        ),
        _ => anyhow::Error::new(e).context("starting the service"),
    }
}

fn wait_for(service: &windows_service::service::Service, state: ServiceState) -> Result<()> {
    let until = Instant::now() + WAIT;
    loop {
        let now = service.query_status()?.current_state;
        if now == state {
            return Ok(());
        }
        if state == ServiceState::Running && now == ServiceState::Stopped {
            bail!("the service stopped while starting");
        }
        if Instant::now() > until {
            bail!("the service is still {now:?} after {} s", WAIT.as_secs());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// How this computer's own accounts appear to another machine: `DOMAIN\HOST$`, or None outside a
/// domain.
pub fn network_name() -> Option<String> {
    let host = std::env::var("COMPUTERNAME").ok()?;
    let mut domain = vec![0u16; 256];
    let (mut sid_len, mut domain_len, mut use_) = (0u32, domain.len() as u32, 0);
    let account = wide(&format!("{host}$"));
    // The first call only asks how big the SID is; the domain is what we want.
    unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            account.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut use_,
        )
    };
    let mut sid = vec![0u8; sid_len as usize];
    let mut domain_len = domain.len() as u32;
    let ok = sid_len > 0
        && unsafe {
            LookupAccountNameW(
                std::ptr::null(),
                account.as_ptr(),
                sid.as_mut_ptr().cast(),
                &mut sid_len,
                domain.as_mut_ptr(),
                &mut domain_len,
                &mut use_,
            )
        } != 0;
    ok.then(|| format!(r"{}\{host}$", String::from_utf16_lossy(&domain[..domain_len as usize])))
}

fn sid_of(name: &str) -> Result<Vec<u8>> {
    // The service manager writes this computer's accounts `.\name`; a lookup needs its name.
    let account = match (name.strip_prefix(r".\"), std::env::var("COMPUTERNAME")) {
        (Some(local), Ok(computer)) => wide(&format!(r"{computer}\{local}")),
        _ => wide(name),
    };
    let (mut sid_len, mut domain_len, mut use_) = (0u32, 0u32, 0);
    unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            account.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_len,
            std::ptr::null_mut(),
            &mut domain_len,
            &mut use_,
        )
    };
    if sid_len == 0 {
        bail!("Windows knows no account named {name}");
    }
    let mut sid = vec![0u8; sid_len as usize];
    let mut domain = vec![0u16; domain_len as usize];
    let ok = unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            account.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut use_,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("looking up the account {name}"));
    }
    Ok(sid)
}

/// The local policy, open for reading and granting rights.
struct Policy(LSA_HANDLE);

impl Policy {
    fn open() -> Result<Policy> {
        let attributes: LSA_OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
        let mut handle: LSA_HANDLE = 0;
        let status = unsafe {
            LsaOpenPolicy(
                std::ptr::null(),
                &attributes,
                (POLICY_LOOKUP_NAMES | POLICY_CREATE_ACCOUNT) as u32,
                &mut handle,
            )
        };
        lsa(status).context("opening the local security policy (run this as an administrator)")?;
        Ok(Policy(handle))
    }
}

impl Drop for Policy {
    fn drop(&mut self) {
        unsafe { LsaClose(self.0) };
    }
}

fn lsa(status: i32) -> std::io::Result<()> {
    if status == 0 {
        return Ok(());
    }
    Err(std::io::Error::from_raw_os_error(unsafe { LsaNtStatusToWinError(status) } as i32))
}

fn grant_logon_right(sid: &[u8]) -> Result<()> {
    let policy = Policy::open()?;
    let mut right = wide(LOGON_RIGHT);
    let name = LSA_UNICODE_STRING {
        Length: (LOGON_RIGHT.len() * 2) as u16,
        MaximumLength: (LOGON_RIGHT.len() * 2) as u16,
        Buffer: right.as_mut_ptr(),
    };
    lsa(unsafe { LsaAddAccountRights(policy.0, sid.as_ptr().cast_mut().cast(), &name, 1) })?;
    Ok(())
}

/// The rights given to the account itself (not through its groups).
fn rights_of(sid: &[u8]) -> Result<Vec<String>> {
    let policy = Policy::open()?;
    let mut rights: *mut LSA_UNICODE_STRING = std::ptr::null_mut();
    let mut count = 0u32;
    let status =
        unsafe { LsaEnumerateAccountRights(policy.0, sid.as_ptr().cast_mut().cast(), &mut rights, &mut count) };
    // STATUS_OBJECT_NAME_NOT_FOUND: no rights of its own at all.
    if status as u32 == 0xC000_0034 {
        return Ok(Vec::new());
    }
    lsa(status)?;
    let list = (0..count as usize)
        .map(|i| unsafe {
            let s = &*rights.add(i);
            String::from_utf16_lossy(std::slice::from_raw_parts(s.Buffer, s.Length as usize / 2))
        })
        .collect();
    unsafe { LsaFreeMemory(rights.cast()) };
    Ok(list)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
