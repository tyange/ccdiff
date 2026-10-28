//! 의사 터미널(PTY)에서 프로그램 하나를 돌린다. 웹 터미널 칸마다 `tmux attach` 를 여기서 돌린다.

use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::process::CommandExt,
    process::{Child, Command},
    ptr,
};

pub struct Pty {
    /// 읽기·쓰기 모두 이 파일로 한다(복제해서 스레드마다 나눠 쓴다).
    pub master: File,
    pub child: Child,
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

impl Pty {
    pub fn spawn(mut cmd: Command, cols: u16, rows: u16) -> io::Result<Self> {
        let (mut master, mut slave) = (-1, -1);
        let mut ws = winsize(cols, rows);
        // SAFETY: 출력 인자는 이 함수의 지역 변수이고, 이름·termios 는 쓰지 않아 null 을 넘긴다.
        if unsafe { libc::openpty(&mut master, &mut slave, ptr::null_mut(), ptr::null_mut(), &mut ws) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openpty 가 막 연 fd 들이고, 이 File 들이 유일한 소유자다.
        let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
        // 자식이 master 를 물려받지 않게 한다 — 남으면 칸을 닫아도 PTY 가 닫히지 않는다.
        // SAFETY: 살아 있는 fd 에 플래그만 바꾼다.
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        cmd.stdin(slave.try_clone()?).stdout(slave.try_clone()?).stderr(slave);
        // SAFETY: fork 뒤 exec 전에는 async-signal-safe 함수(setsid·ioctl)만 부른다.
        unsafe {
            cmd.pre_exec(|| {
                // 새 세션을 열고 PTY 를 제어 터미널로 삼아야 tmux 가 크기 변경(SIGWINCH)을 받는다.
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        Ok(Self { master, child })
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let ws = winsize(cols, rows);
        // SAFETY: master 는 살아 있는 PTY fd 이고 ws 는 지역 변수다.
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
