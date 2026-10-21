//! Development-only ownership of a child engine. Never stop a pre-existing daemon.
use std::{io,os::unix::{net::UnixStream,process::CommandExt},path::Path,process::{Child,Command},time::{Duration,Instant}};

pub struct OwnedService{child:Child}
impl OwnedService{
    pub fn start()->io::Result<Self>{
        let binary=std::env::current_exe()?.with_file_name("alixerd");
        if !binary.is_file(){return Err(io::Error::new(io::ErrorKind::NotFound,"Cannot find sibling alixerd binary; run cargo build --workspace first"));}
        Self::start_command(Command::new(binary),&alixer_protocol::runtime_dir().join("control.sock"))
    }
    fn start_command(command:Command,socket:&Path)->io::Result<Self>{
        if UnixStream::connect(socket).is_ok(){return Err(io::Error::new(io::ErrorKind::AlreadyExists,"A Alixer service is already running. Stop it before using --dev-service; it will not be terminated by the GUI."));}
        let mut owned=Self::spawn(command)?;
        let deadline=Instant::now()+Duration::from_secs(10);
        loop{
            if let Some(status)=owned.child.try_wait()?{return Err(io::Error::other(format!("Development service exited during startup: {status}")));}
            if UnixStream::connect(socket).is_ok(){return Ok(owned);}
            if Instant::now()>=deadline{return Err(io::Error::new(io::ErrorKind::TimedOut,"Development service did not create its control socket"));}
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    fn spawn(mut command:Command)->io::Result<Self>{
        let parent=std::process::id();
        // SAFETY: only async-signal-safe libc operations run between fork and exec.
        // Parent-death SIGTERM also cleans up if the GUI crashes or is SIGKILLed.
        unsafe{command.pre_exec(move||{
            if libc::prctl(libc::PR_SET_PDEATHSIG,libc::SIGTERM)!=0{return Err(io::Error::last_os_error());}
            if libc::getppid()!=parent as libc::pid_t{return Err(io::Error::other("GUI exited before service startup"));}
            Ok(())
        });}
        Ok(Self{child:command.spawn()?})
    }
}
impl Drop for OwnedService{
    fn drop(&mut self){
        if self.child.try_wait().ok().flatten().is_some(){return;}
        // SAFETY: this PID is still owned by our unreaped Child and cannot be reused.
        unsafe{libc::kill(self.child.id() as libc::pid_t,libc::SIGTERM);}
        let deadline=Instant::now()+Duration::from_secs(5);
        while Instant::now()<deadline{
            if self.child.try_wait().ok().flatten().is_some(){return;}
            std::thread::sleep(Duration::from_millis(20));
        }
        let _=self.child.kill();let _=self.child.wait();
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn dropping_the_guard_terminates_and_reaps_its_child(){
        let mut command=Command::new("sleep");command.arg("60");
        let service=OwnedService::spawn(command).unwrap();let pid=service.child.id();drop(service);
        assert_eq!(unsafe{libc::kill(pid as libc::pid_t,0)},-1);
        assert_eq!(io::Error::last_os_error().raw_os_error(),Some(libc::ESRCH));
    }
    #[test]
    fn existing_service_is_left_running(){
        let dir=std::env::temp_dir().join(format!("alixer-owned-{}",alixer_core::Id::new_v4()));std::fs::create_dir_all(&dir).unwrap();
        let path=dir.join("socket");let listener=std::os::unix::net::UnixListener::bind(&path).unwrap();
        let error=OwnedService::start_command(Command::new("true"),&path).err().expect("must refuse existing service");
        assert_eq!(error.kind(),io::ErrorKind::AlreadyExists);assert!(UnixStream::connect(&path).is_ok());
        drop(listener);std::fs::remove_dir_all(dir).unwrap();
    }
}
