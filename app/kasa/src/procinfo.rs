//! 칸 셸의 지금 폴더. 셸 통합(OSC 7)이 없는 셸도 새 칸이 같은 자리에서 열리게, 셸 프로세스의
//! 폴더를 직접 읽는다.

use std::path::PathBuf;

#[cfg(target_os = "macos")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n != size {
        return None;
    }
    // vip_path 는 [[c_char; 32]; 32] 로 쪼개 선언돼 있지만 메모리는 이어진 1024바이트다.
    let raw = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr() as *const libc::c_char) };
    let path = PathBuf::from(raw.to_str().ok()?);
    path.is_dir().then_some(path)
}

#[cfg(target_os = "linux")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok().filter(|p| p.is_dir())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn reads_own_cwd() {
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        let got = super::process_cwd(std::process::id()).unwrap().canonicalize().unwrap();
        assert_eq!(got, here);
    }
}
