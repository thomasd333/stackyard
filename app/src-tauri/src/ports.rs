//! Port availability checks and finding which process holds a port.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::AF_INET;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

/// True when nothing listens on `port` (checked by binding on IPv4 and IPv6, like Apache does).
pub fn is_free(port: u16) -> bool {
    TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok()
        && TcpListener::bind((Ipv6Addr::UNSPECIFIED, port)).is_ok()
}

/// True when something accepts connections on localhost:`port`.
pub fn is_listening(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// Human-readable description of what blocks `port`, e.g. "httpd.exe (PID 1234)".
pub fn describe_owner(port: u16) -> String {
    match owner_pid(port) {
        Some(4) => "System (PID 4) - usually IIS or another service using http.sys".into(),
        Some(pid) => match process_name(pid) {
            Some(name) => format!("{name} (PID {pid})"),
            None => format!("PID {pid}"),
        },
        None => "another program".into(),
    }
}

fn owner_pid(port: u16) -> Option<u32> {
    unsafe {
        let mut size = 0u32;
        GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET as u32, TCP_TABLE_OWNER_PID_LISTENER, 0);
        // u32 buffer keeps the table properly aligned.
        let mut buf = vec![0u32; size as usize / 4 + 1];
        let rc = GetExtendedTcpTable(
            buf.as_mut_ptr().cast(),
            &mut size,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
        if rc != 0 {
            return None;
        }
        let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        let rows = std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
        rows.iter()
            .find(|r| u16::from_be(r.dwLocalPort as u16) == port)
            .map(|r| r.dwOwningPid)
    }
}

fn process_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned())
    }
}
