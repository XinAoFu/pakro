use super::*;
use std::{
    fs::{self, File},
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    process::{Command, Stdio},
};

const WORKER: &str = r#"
#define _DEFAULT_SOURCE
#include <sys/types.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <arpa/inet.h>
#include <spawn.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
extern char **environ;
#ifndef P6_COMPONENT_VARIANT
#define P6_COMPONENT_VARIANT 1
#endif
static volatile int fixture_image_variant = P6_COMPONENT_VARIANT;
static int req(char op, const char *path, unsigned counter) {
  char tmp[4096], request[4096], response[4096];
  const char *requests=getenv("PAKRO_NATIVE_REQUESTS"), *responses=getenv("PAKRO_NATIVE_RESPONSES");
  if (!requests || !responses) return -1;
  snprintf(tmp,sizeof(tmp),"%s/%ld-%u.tmp",requests,(long)getpid(),counter);
  snprintf(request,sizeof(request),"%s/%ld-%u.req",requests,(long)getpid(),counter);
  snprintf(response,sizeof(response),"%s/%ld-%u.resp",responses,(long)getpid(),counter);
  FILE *f=fopen(tmp,"wb"); if(!f)return -1;
  fputc(op,f); fputc(0,f); fwrite(path,1,strlen(path),f); fclose(f);
  if(rename(tmp,request))return -1;
  for(int i=0;i<500;i++) { f=fopen(response,"rb"); if(f) { int status=fgetc(f); fclose(f); return status; } usleep(10000); }
  return -1;
}
static int copy(const char *from,const char *to) {
  int a=open(from,O_RDONLY),b=open(to,O_WRONLY|O_CREAT|O_TRUNC,0755); if(a<0||b<0)return 1;
  char bytes[8192]; ssize_t n; while((n=read(a,bytes,sizeof(bytes)))>0)if(write(b,bytes,n)!=n)return 1;
  close(a);close(b);return n<0;
}
static int execute(const char *path,int deny) {
  pid_t child=fork(); if(child<0)return 1;
  if(!child) { char *args[]={(char*)path,"leaf",NULL}; execv(path,args); _exit(deny && (errno==EACCES||errno==EPERM)?93:90); }
  int status; if(waitpid(child,&status,0)!=child)return 1;
  return !WIFEXITED(status)||WEXITSTATUS(status)!=(deny?93:0);
}
static int component_spawn(const char *path,char *const args[]) {
  pid_t child;int error=posix_spawn(&child,path,NULL,NULL,args,environ);
  if(error){fprintf(stderr,"component posix_spawn: %s\n",strerror(error));return 1;}
  int status;if(waitpid(child,&status,0)!=child)return 1;
  if(!WIFEXITED(status)||WEXITSTATUS(status)){fprintf(stderr,"component child status: %d\n",status);return 1;}
  return 0;
}
int main(int argc,char **argv) {
  if(argc<2)return 2;
  if(!strcmp(argv[1],"leaf")){puts("leaf");return 0;}
  if(!strcmp(argv[1],"deny-program")) {
    if(argc!=3||execute(argv[2],1))return 54;
    puts("unknown-pool-denied");return 0;
  }
  if(!strcmp(argv[1],"component-leaf")) {
    if(argc!=3)return 30;
    int marker=open(argv[2],O_CREAT|O_EXCL|O_WRONLY,0600);if(marker<0)return 31;
    if(write(marker,"component-leaf-ran\n",19)!=19)return 32;close(marker);
    puts("component-leaf");printf("component-variant=%d\n",fixture_image_variant);return 0;
  }
  if(!strcmp(argv[1],"fake-compiler")) {
    /* Component fixture only: copy a known executable, never compile source. */
    if(argc!=6||strcmp(argv[4],"-o"))return 33;
    const char *gate=getenv("PAKRO_NATIVE_GATE_LIBRARY");
#ifdef __APPLE__
    const char *injection=getenv("DYLD_INSERT_LIBRARIES");
#else
    const char *injection=getenv("LD_PRELOAD");
#endif
    if(!gate||!injection||strcmp(gate,injection))return 34;
    const char *secret=getenv("PAKRO_COMPONENT_SECRET"),*outside=getenv("PAKRO_COMPONENT_OUTSIDE");
    if(!secret||!outside)return 35;
    int forbidden=open(secret,O_RDONLY);if(forbidden>=0){close(forbidden);return 36;}
    if(errno!=EACCES&&errno!=EPERM)return 37;
    forbidden=open(outside,O_WRONLY|O_CREAT,0600);if(forbidden>=0){close(forbidden);return 38;}
    if(errno!=EACCES&&errno!=EPERM)return 39;
    const char *port=getenv("PAKRO_COMPONENT_TCP_PORT");if(!port)return 40;
    int network=socket(AF_INET,SOCK_STREAM,0);
    if(network>=0){
      struct sockaddr_in address={0};address.sin_family=AF_INET;
      address.sin_addr.s_addr=htonl(INADDR_LOOPBACK);address.sin_port=htons((unsigned short)atoi(port));
      if(connect(network,(struct sockaddr*)&address,sizeof(address))==0)return 41;
      if(errno!=EACCES&&errno!=EPERM)return 42;close(network);
    }else if(errno!=EACCES&&errno!=EPERM)return 43;
    /* This spawn emits another E through the actually restored exec gate. */
    char *probe[]={argv[2],"leaf",NULL};if(component_spawn(argv[2],probe))return 44;
    if(copy(argv[2],argv[5]))return 45;
    struct stat output;if(stat(argv[5],&output))return 46;
    FILE *marker=fopen(argv[3],"wx");if(!marker)return 47;
    fprintf(marker,"fixture-image-copy-only\n%llu:%llu\n",(unsigned long long)output.st_dev,(unsigned long long)output.st_ino);
    fclose(marker);puts("fixture-image-copy-only");return 0;
  }
  if(!strcmp(argv[1],"gate-compile")) {
    if(argc!=5)return 48;
    char *compiler[]={argv[2],"-o",argv[3],NULL};
    if(component_spawn(argv[2],compiler))return 49;
    struct stat output;if(lstat(argv[3],&output)||!S_ISLNK(output.st_mode))return 50;
    pid_t child=fork();if(child<0)return 51;
    if(!child){char *leaf[]={argv[3],"component-leaf",argv[4],NULL};execv(argv[3],leaf);perror("component leaf exec");_exit(52);}
    int status;if(waitpid(child,&status,0)!=child||!WIFEXITED(status)||WEXITSTATUS(status))return 53;
    puts("component-posix-spawn");return 0;
  }
  if(!strcmp(argv[1],"isolation")) {
    if(argc!=6)return 3;
    int a=open(argv[2],O_RDONLY); if(a>=0)return 4;
    int b=open(argv[3],O_CREAT|O_WRONLY,0600); if(b>=0)return 5;
    int n=socket(AF_INET,SOCK_STREAM,0),u=socket(AF_UNIX,SOCK_STREAM,0);
    if(n>=0) {
      struct sockaddr_in address={0};address.sin_family=AF_INET;
      address.sin_addr.s_addr=htonl(INADDR_LOOPBACK);address.sin_port=htons((unsigned short)atoi(argv[4]));
      if(connect(n,(struct sockaddr*)&address,sizeof(address))==0)return 6;
      if(errno!=EACCES&&errno!=EPERM)return 8;close(n);
    }
    if(u>=0) {
      struct sockaddr_un address={0};address.sun_family=AF_UNIX;
      if(strlen(argv[5])>=sizeof(address.sun_path))return 9;strcpy(address.sun_path,argv[5]);
      if(connect(u,(struct sockaddr*)&address,sizeof(address))==0)return 6;
      if(errno!=EACCES&&errno!=EPERM)return 8;close(u);
    }
    if(execute("/bin/echo",1))return 7;
    puts("isolated");return 0;
  }
  if(!strcmp(argv[1],"register")) {
    if(argc!=4||copy(argv[2],argv[3]))return 10;
    if(execute(argv[3],1))return 11;
    if(req('R',argv[3],1)!=0)return 12;
    struct stat info;if(lstat(argv[3],&info)||!S_ISLNK(info.st_mode))return 13;
    if(execute(argv[3],0))return 14;
    char object[4096];snprintf(object,sizeof(object),"%s.o",argv[3]);
    int fd=open(object,O_CREAT|O_WRONLY,0600);if(fd<0)return 15;write(fd,"object",6);close(fd);
    if(req('R',object,2)!=2||req('E',object,3)!=1)return 16;
    puts("registered");return 0;
  }
  if(argc!=3)return 20;
  int ticks=open(argv[2],O_WRONLY|O_APPEND);if(ticks<0)return 21;
  int ready[2];if(pipe(ready))return 22;
  pid_t child=fork();if(child<0)return 23;
  if(!child) {
    close(ready[0]);if(setpgid(0,0))_exit(24);
    write(ticks,"x",1);write(ready[1],"r",1);close(ready[1]);
    for(;;){write(ticks,"x",1);usleep(10000);}
  }
  close(ready[1]);char byte;if(read(ready[0],&byte,1)!=1)return 25;close(ready[0]);close(ticks);
  if(!strcmp(argv[1],"normal")){puts("done");return 0;}
  if(!strcmp(argv[1],"limit")){char out[8192];memset(out,'x',sizeof(out));for(;;)write(1,out,sizeof(out));}
  for(;;)pause();
}
"#;

struct Fixture {
    _root: tempfile::TempDir,
    config: NativeBuildConfig,
    worker: PathBuf,
    ticks: File,
    tick_path: PathBuf,
    secret: PathBuf,
    outside: PathBuf,
}

fn compile_fixture_image(source: &Path, output: &Path, variant: u8) {
    let compiler = std::env::var_os("PAKRO_TEST_NATIVE_CC").map(PathBuf::from).unwrap_or_else(|| "/usr/bin/cc".into());
    let result = Command::new(compiler).args(["-std=c11", "-O0"])
        .arg(format!("-DP6_COMPONENT_VARIANT={variant}"))
        .arg(source).arg("-o").arg(output).env_clear().env("PATH", "/usr/bin:/bin").output().unwrap();
    assert!(result.status.success(), "fixture image compiler failed: {}", String::from_utf8_lossy(&result.stderr));
}

struct ExecComponents {
    bin: PathBuf,
    wrapper: PathBuf,
    gate: PathBuf,
    compiler_marker: PathBuf,
    leaf_marker: PathBuf,
    generated: PathBuf,
    environment: BTreeMap<String, String>,
    _peer: std::net::TcpListener,
}

impl Drop for ExecComponents {
    fn drop(&mut self) {
        // The fixture owner can clean its OS-readonly input directory afterwards.
        let _ = fs::set_permissions(&self.bin, fs::Permissions::from_mode(0o700));
    }
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let source = root_path.join("worker.c");
        fs::write(&source, WORKER).unwrap();
        let worker = root_path.join("worker");
        compile_fixture_image(&source, &worker, 1);
        let workspace = root_path.join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
        let work = workspace.join("work");
        fs::create_dir(&work).unwrap();
        let tick_path = work.join("ticks");
        fs::write(&tick_path, []).unwrap();
        let ticks = File::open(&tick_path).unwrap();
        let secret = root_path.join("secret");
        fs::write(&secret, b"private outside bytes").unwrap();
        let outside = root_path.join("forbidden-output");
        let bootstrap = PathBuf::from(
            std::env::var_os("PAKRO_TEST_BINARY")
                .expect("PAKRO_TEST_BINARY must name the newly built main"),
        );
        assert!(bootstrap.is_absolute() && bootstrap.is_file());
        let mut reads = vec![worker.clone(), workspace.clone()];
        for path in [
            "/lib",
            "/lib64",
            "/usr/lib",
            "/System/Library",
            "/dev/null",
            "/dev/urandom",
        ] {
            if Path::new(path).exists() {
                reads.push(PathBuf::from(path));
            }
        }
        let config = NativeBuildConfig {
            bootstrap,
            fixed_programs: vec![worker.clone()],
            sandbox: SandboxConfig {
                allow_read: reads,
                allow_write: vec![work.clone()],
                ..Default::default()
            },
            workspace: workspace.clone(),
            exec_pool: workspace.join("sealed"),
            generated_roots: vec![work],
            requests: workspace.join("requests"),
            responses: workspace.join("responses"),
            unshare: Some("/usr/bin/unshare".into()),
        };
        Self {
            _root: root,
            config,
            worker,
            ticks,
            tick_path,
            secret,
            outside,
        }
    }

    async fn stopped(&self, removed: bool) {
        if removed {
            let until = Instant::now() + Duration::from_secs(3);
            while self.config.workspace.exists() && Instant::now() < until {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(
                !self.config.workspace.exists(),
                "private workspace was not cleaned"
            );
        }
        let before = self.ticks.metadata().unwrap().len();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            before,
            self.ticks.metadata().unwrap().len(),
            "grandchild continued writing after cleanup"
        );
    }

    fn exec_components(&mut self) -> ExecComponents {
        let bin = self.worker.parent().unwrap().join("readonly-fixtures-bin");
        fs::create_dir(&bin).unwrap();
        let gate = bin.join(crate::native_exec::GATE_LIBRARY_NAME);
        let wrapper = bin.join("cc");
        fs::write(&gate, crate::native_exec::GATE_LIBRARY_BYTES).unwrap();
        fs::write(&wrapper, crate::native_exec::COMPILER_WRAPPER_BYTES).unwrap();
        fs::set_permissions(&gate, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o555)).unwrap();
        self.config.fixed_programs.push(wrapper.clone());
        self.config.sandbox.allow_read.push(bin.clone());
        let work = &self.config.generated_roots[0];
        let compiler_marker = work.join("component-compiler-marker");
        let leaf_marker = work.join("component-leaf-marker");
        let generated = work.join("component-generated");
        let peer = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std::net::TcpStream::connect(peer.local_addr().unwrap()).unwrap();
        let mut environment = BTreeMap::from([
            ("PAKRO_NATIVE_GATE_LIBRARY".into(), gate.to_string_lossy().into_owned()),
            ("PAKRO_NATIVE_REAL_CC".into(), self.worker.to_string_lossy().into_owned()),
            ("PAKRO_NATIVE_CC_ARGS".into(), ["fake-compiler".to_owned(), self.worker.to_string_lossy().into_owned(), compiler_marker.to_string_lossy().into_owned()].join("\u{1f}")),
            ("PAKRO_COMPONENT_SECRET".into(), self.secret.to_string_lossy().into_owned()),
            ("PAKRO_COMPONENT_OUTSIDE".into(), self.outside.to_string_lossy().into_owned()),
            ("PAKRO_COMPONENT_TCP_PORT".into(), peer.local_addr().unwrap().port().to_string()),
        ]);
        environment.insert(if cfg!(target_os="macos") { "DYLD_INSERT_LIBRARIES" } else { "LD_PRELOAD" }.into(), gate.to_string_lossy().into_owned());
        ExecComponents { bin, wrapper, gate, compiler_marker, leaf_marker, generated, environment, _peer: peer }
    }

    fn assert_exec_component(&self, session: &NativeBuildSession, components: &ExecComponents) -> Vec<Vec<u8>> {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::read(&components.leaf_marker).unwrap(), b"component-leaf-ran\n");
        let marker = fs::read_to_string(&components.compiler_marker).unwrap();
        let mut lines = marker.lines();
        assert_eq!(lines.next(), Some("fixture-image-copy-only"));
        let (device, inode) = lines.next().unwrap().split_once(':').unwrap();
        let output_identity = (device.parse::<u64>().unwrap(), inode.parse::<u64>().unwrap());
        assert!(fs::symlink_metadata(&components.generated).unwrap().file_type().is_symlink());
        let sealed = components.generated.canonicalize().unwrap();
        assert!(sealed.starts_with(session.exec_pool()));
        let sealed_metadata = fs::metadata(&sealed).unwrap();
        assert_ne!((sealed_metadata.dev(), sealed_metadata.ino()), output_identity, "R must copy the generated image to a new inode");
        assert_eq!(fs::read(&sealed).unwrap(), fs::read(&self.worker).unwrap());
        assert_eq!(sealed_metadata.mode() & 0o222, 0);
        assert_eq!(fs::metadata(&components.gate).unwrap().mode() & 0o222, 0);
        assert_eq!(fs::metadata(&components.wrapper).unwrap().mode() & 0o222, 0);
        assert_eq!(fs::read(&components.gate).unwrap(), crate::native_exec::GATE_LIBRARY_BYTES);
        assert_eq!(fs::read(&components.wrapper).unwrap(), crate::native_exec::COMPILER_WRAPPER_BYTES);
        assert!(!self.outside.exists());
        assert_eq!(fs::read(&self.secret).unwrap(), b"private outside bytes");
        assert_eq!(fs::read_dir(session.requests()).unwrap().count(), 0);
        fs::read_dir(session.responses()).unwrap().map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "resp"))
            .map(|path| fs::read(path).unwrap()).collect()
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "explicit GNU make RESETIDS identity diagnostic, never backend acceptance"]
async fn p6_linux_make_spawn_identity_diagnostic() {
    let mut fixture = Fixture::new();
    let source = fixture.worker.with_file_name("make-spawn-identity.c");
    let helper = fixture.worker.with_file_name("make-spawn-identity");
    fs::write(&source, r#"
#define _GNU_SOURCE
#include <spawn.h>
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>
#include <stdio.h>
extern char **environ;
int main(int argc,char **argv) {
  if(argc!=2)return 2;
  printf("ids uid=%u euid=%u gid=%u egid=%u\n",getuid(),geteuid(),getgid(),getegid());
  for(int search=0;search<2;search++)for(int reset=0;reset<2;reset++) {
    posix_spawnattr_t attr;sigset_t signals;sigemptyset(&signals);
    if(posix_spawnattr_init(&attr))return 3;
    if(posix_spawnattr_setsigmask(&attr,&signals)||posix_spawnattr_setsigdefault(&attr,&signals))return 4;
    short flags=POSIX_SPAWN_SETSIGMASK|POSIX_SPAWN_SETSIGDEF;
    if(reset)flags|=POSIX_SPAWN_RESETIDS;
    if(posix_spawnattr_setflags(&attr,flags))return 5;
    char *args[]={argv[1],"-c","printf 'identity-child=42\\n'",NULL};
    pid_t pid;int error=search?posix_spawnp(&pid,"sh",NULL,&attr,args,environ):posix_spawn(&pid,argv[1],NULL,&attr,args,environ);
    printf("kind=%s reset=%d flags=%d error=%d\n",search?"spawnp":"spawn",reset,flags,error);fflush(stdout);
    if(!error){int status;if(waitpid(pid,&status,0)!=pid||!WIFEXITED(status)||WEXITSTATUS(status))return 6;}
    posix_spawnattr_destroy(&attr);
  }
  return 0;
}
"#).unwrap();
    compile_fixture_image(&source, &helper, 1);
    let shell = PathBuf::from("/bin/sh").canonicalize().unwrap();
    let outside = Command::new(&helper).arg(&shell).env_clear().env("PATH", "/usr/bin:/bin").output().unwrap();
    eprintln!("P6_MAKE_IDENTITY_OUTSIDE status={} stdout={} stderr={}", outside.status,
        String::from_utf8_lossy(&outside.stdout), String::from_utf8_lossy(&outside.stderr));
    assert!(outside.status.success());
    assert_eq!(String::from_utf8_lossy(&outside.stdout).lines().filter(|line| line.starts_with("kind=") && line.ends_with("error=0")).count(), 4);
    let components = fixture.exec_components();
    fixture.config.fixed_programs.extend([helper.clone(), shell.clone()]);
    fixture.config.sandbox.allow_read.extend([helper.clone(), shell.clone()]);
    let make = std::env::var_os("PAKRO_TEST_NATIVE_MAKE").map(PathBuf::from).map(|path| path.canonicalize().unwrap());
    if let Some(make) = &make { fixture.config.fixed_programs.push(make.clone()); fixture.config.sandbox.allow_read.push(make.clone()); }
    let work = fixture.config.generated_roots[0].clone();
    let environment = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("TMPDIR".into(), work.to_string_lossy().into_owned()),
        ("LD_PRELOAD".into(), components.gate.to_string_lossy().into_owned()),
        ("PAKRO_NATIVE_GATE_LIBRARY".into(), components.gate.to_string_lossy().into_owned()),
    ]);
    let mut session = NativeBuildSession::start(fixture.config.clone()).await.unwrap();
    let cancel = CancellationToken::new();
    let output = session.run(&helper, &[shell.to_string_lossy().into_owned()], &work, &environment,
        Instant::now()+Duration::from_secs(10), 64*1024, &cancel).await.unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprintln!("P6_MAKE_IDENTITY_NATIVE status={} stdout={} stderr={}", output.status, stdout, String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success());
    assert_eq!(stdout.lines().filter(|line| line.starts_with("kind=") && line.contains("reset=0 ") && line.ends_with("error=0")).count(), 2);
    assert_eq!(stdout.lines().filter(|line| line.starts_with("kind=") && line.contains("reset=1 ") && line.ends_with("error=22")).count(), 2);
    if let Some(make) = make {
        let makefile = work.join("identity.mk");
        fs::write(&makefile, ".PHONY: all\nall:\n\t@printf 'actual-make-child=42\\n'; :\n").unwrap();
        let output = session.run(&make, &["-j1".into(), "-f".into(), makefile.to_string_lossy().into_owned()],
            &work, &environment, Instant::now()+Duration::from_secs(10), 64*1024, &cancel).await.unwrap();
        eprintln!("P6_ACTUAL_MAKE_SMOKE program={} status={} stdout={} stderr={}", make.display(), output.status,
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("actual-make-child=42"));
    }
    session.close().await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "explicit installed Rust/Cargo Linux spawn diagnostic, never backend acceptance"]
async fn p6_linux_rust_spawn_ipc_diagnostic() {
    use std::os::fd::AsRawFd;
    let mut fixture = Fixture::new();
    let rustc = PathBuf::from(std::env::var_os("PAKRO_TEST_NATIVE_RUSTC").expect("explicit installed rustc is required")).canonicalize().unwrap();
    let cargo = PathBuf::from(std::env::var_os("PAKRO_TEST_NATIVE_CARGO").expect("explicit installed cargo is required")).canonicalize().unwrap();
    let toolchain = rustc.parent().unwrap().parent().unwrap().to_owned();
    let helper_source = fixture.worker.parent().unwrap().join("rust-spawn-probe.rs");
    let helper = fixture.worker.parent().unwrap().join("rust-spawn-probe");
    fs::write(&helper_source, r#"
use std::os::unix::process::CommandExt;
use std::process::Command;
unsafe extern "C" {
    fn socketpair(domain:i32, kind:i32, protocol:i32, pair:*mut i32)->i32;
    fn close(fd:i32)->i32; fn fork()->i32; fn waitpid(pid:i32,status:*mut i32,options:i32)->i32; fn _exit(code:i32)->!;
    fn recv(fd:i32,buf:*mut u8,len:usize,flags:i32)->isize;
    fn recvfrom(fd:i32,buf:*mut u8,len:usize,flags:i32,source:*mut u8,source_len:*mut u32)->isize;
    fn read(fd:i32,buf:*mut u8,len:usize)->isize; fn write(fd:i32,buf:*const u8,len:usize)->isize;
}
fn main() {
    let args:Vec<String>=std::env::args().collect();
    println!("helper_ld_preload={:?}",std::env::var("LD_PRELOAD"));
    if args[1]=="fd-wait" {std::thread::sleep(std::time::Duration::from_secs(1));return;}
    if args[1].starts_with("socketpair") {
        let (family,kind,protocol)=match args[1].as_str() {
            "socketpair-inet"=>(2,5|0o2000000,0),
            "socketpair-stream"=>(1,1|0o2000000,0),
            "socketpair-no-cloexec"=>(1,5,0),
            "socketpair-nonblock"=>(1,5|0o2000000|0o4000,0),
            "socketpair-protocol"=>(1,5|0o2000000,1),
            _=>(1,5|0o2000000,0),
        };
        let mut pair=[-1,-1];
        let result=unsafe{socketpair(family,kind,protocol,pair.as_mut_ptr())};
        let errno=if result<0 {std::io::Error::last_os_error().raw_os_error().unwrap_or(0)}else{0};
        println!("socketpair_family={family} kind_full={kind} protocol={protocol} result={result} errno={errno}");
        if result==0 {
            let child=unsafe{fork()};
            if child==0 {
                unsafe{close(pair[0]);}
                let error_packet=[0,0,0,2,b'N',b'O',b'E',b'X'];
                for _ in 0..3 {
                    if unsafe{write(pair[1],error_packet.as_ptr(),error_packet.len())}!=8 {unsafe{_exit(96);}}
                }
                unsafe{close(pair[1]);_exit(0);}
            }
            assert!(child>0);
            unsafe{close(pair[1]);}pair[1]=-1;
            let mut data=[0u8;8];
            let mut address=[0u8;128];let mut address_len=address.len() as u32;
            for (label,flags,peer,length) in [
                ("flags-peek",2,std::ptr::null_mut(),std::ptr::null_mut()),
                ("nonnull-source",0,address.as_mut_ptr(),std::ptr::null_mut()),
                ("nonnull-length",0,std::ptr::null_mut(),&mut address_len as *mut u32),
                ("nonnull-both",0,address.as_mut_ptr(),&mut address_len as *mut u32),
            ] {
                let result=unsafe{recvfrom(pair[0],data.as_mut_ptr(),data.len(),flags,peer,length)};
                let errno=if result<0 {std::io::Error::last_os_error().raw_os_error().unwrap_or(0)}else{0};
                println!("negative_recvfrom {label} flags={flags} source_nonnull={} length_nonnull={} result={result} errno={errno}",!peer.is_null(),!length.is_null());
            }
            let received=unsafe{recv(pair[0],data.as_mut_ptr(),data.len(),0)};
            let recv_errno=if received<0 {std::io::Error::last_os_error().raw_os_error().unwrap_or(0)}else{0};
            println!("paired_recv flags=0 result={received} errno={recv_errno}");
            let received=unsafe{recvfrom(pair[0],data.as_mut_ptr(),data.len(),0,std::ptr::null_mut(),std::ptr::null_mut())};
            let recvfrom_errno=if received<0 {std::io::Error::last_os_error().raw_os_error().unwrap_or(0)}else{0};
            println!("paired_recvfrom flags=0 source=NULL source_len=NULL result={received} errno={recvfrom_errno}");
            let read_count=unsafe{read(pair[0],data.as_mut_ptr(),data.len())};
            let mut status=0;let waited=unsafe{waitpid(child,&mut status,0)};
            println!("paired_read result={read_count} packet={data:?} child_waited={waited} child_status={status}");
            unsafe{close(pair[0]);}
        }
        return;
    }
    let mut command=Command::new(&args[2]);command.arg("--version");
    if args[1].starts_with("preexec") {unsafe{command.pre_exec(||Ok(()));}}
    match command.output() {
        Ok(output)=>println!("spawn_exit={:?} stdout={} stderr={}",output.status.code(),String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr)),
        Err(error)=>println!("spawn_errno={:?} error={error}",error.raw_os_error()),
    }
}
"#).unwrap();
    let built = Command::new(&rustc).args(["--edition=2021", "-C", "debuginfo=0"])
        .arg(&helper_source).arg("-o").arg(&helper).output().unwrap();
    assert!(built.status.success(), "diagnostic helper compile: {}", String::from_utf8_lossy(&built.stderr));
    let components = fixture.exec_components();
    fs::set_permissions(&components.bin, fs::Permissions::from_mode(0o700)).unwrap();
    let wrapper = components.bin.join("rustc");
    fs::write(&wrapper, crate::native_exec::COMPILER_WRAPPER_BYTES).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(&components.bin, fs::Permissions::from_mode(0o555)).unwrap();
    fixture.config.fixed_programs.extend([helper.clone(), cargo.clone(), rustc.clone(), wrapper.clone()]);
    fixture.config.sandbox.allow_read.extend([helper.clone(), toolchain.clone()]);
    let work = fixture.config.generated_roots[0].clone();
    for name in ["cargo-home", "home", "tmp", "project/src"] { fs::create_dir_all(work.join(name)).unwrap(); }
    fs::write(work.join("project/Cargo.toml"), "[package]\nname='p6_spawn_probe'\nversion='0.1.0'\nedition='2021'\n[workspace]\n").unwrap();
    fs::write(work.join("project/Cargo.lock"), "version = 4\n[[package]]\nname = 'p6_spawn_probe'\nversion = '0.1.0'\n").unwrap();
    fs::write(work.join("project/src/lib.rs"), "pub fn answer()->i32{42}\n").unwrap();
    let environment = BTreeMap::from([
        ("PATH".into(), format!("{}:{}:/usr/bin:/bin", components.bin.display(), rustc.parent().unwrap().display())),
        ("HOME".into(), work.join("home").to_string_lossy().into_owned()),
        ("TMPDIR".into(), work.join("tmp").to_string_lossy().into_owned()),
        ("CARGO_HOME".into(), work.join("cargo-home").to_string_lossy().into_owned()),
        ("CARGO_BUILD_JOBS".into(), "1".into()),
        ("RUSTC".into(), wrapper.to_string_lossy().into_owned()),
        ("PAKRO_NATIVE_GATE_LIBRARY".into(), components.gate.to_string_lossy().into_owned()),
        ("PAKRO_NATIVE_REAL_RUSTC".into(), rustc.to_string_lossy().into_owned()),
        ("LD_PRELOAD".into(), components.gate.to_string_lossy().into_owned()),
        ("LD_DEBUG".into(), "libs".into()),
        ("OPENSSL_CONF".into(), "/dev/null".into()),
    ]);
    let mut session = NativeBuildSession::start(fixture.config.clone()).await.unwrap();
    assert!(session.program_identities().iter().any(|program| program.path == wrapper.canonicalize().unwrap()));
    assert!(!session.config.sandbox.runtime_loaders.is_empty());
    assert!(session.config.sandbox.runtime_loaders.iter().all(|loader|
        session.program_identities().iter().any(|program| program.path == *loader)));
    eprintln!("P6_RUST_SPAWN_FIXED wrapper={} loaders={:?}", wrapper.display(), session.config.sandbox.runtime_loaders);
    let peer_flags = unsafe { nix::libc::fcntl(components._peer.as_raw_fd(), nix::libc::F_GETFD) };
    assert!(peer_flags & nix::libc::FD_CLOEXEC != 0);
    eprintln!("P6_RUST_PARENT_NETWORK_FD fd={} cloexec=true", components._peer.as_raw_fd());
    let deadline = Instant::now() + Duration::from_secs(45);
    let watcher = session._watcher.id().expect("live native watcher");
    let probe_args = vec!["fd-wait".into()];
    let cancel = CancellationToken::new();
    let run = session.run(&helper, &probe_args, &work, &environment, deadline, 512 * 1024, &cancel);
    let inspect = async {
        for _ in 0..80 {
            let mut pending = vec![watcher];
            let mut seen = std::collections::BTreeSet::new();
            while let Some(pid) = pending.pop() {
                if !seen.insert(pid) || seen.len() > 32 { continue; }
                if pid != watcher && fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|path| path == helper) {
                    let mut descriptors = Vec::new();
                    for entry in fs::read_dir(format!("/proc/{pid}/fd")).unwrap() {
                        let entry = entry.unwrap();
                        if let Ok(target) = fs::read_link(entry.path()) {
                            descriptors.push((entry.file_name().to_string_lossy().into_owned(), target.to_string_lossy().into_owned()));
                        }
                    }
                    return descriptors;
                }
                if let Ok(children) = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
                    pending.extend(children.split_whitespace().filter_map(|pid| pid.parse::<u32>().ok()));
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("owned native helper FD table was not observed");
    };
    let (run, descriptors) = tokio::join!(run, inspect);
    assert!(run.unwrap().status.success());
    eprintln!("P6_RUST_NATIVE_STDIO_FDS {descriptors:?}");
    assert!(descriptors.iter().any(|(fd, path)| fd == "0" && path == "/dev/null"));
    for fd in ["1", "2"] { assert!(descriptors.iter().any(|(actual, path)| actual == fd && path.starts_with("pipe:["))); }
    assert!(!descriptors.iter().any(|(_, path)| path.starts_with("socket:[")), "native helper inherited a network socket");
    let modes = [
        ("direct-wrapper", wrapper.clone(), vec!["--version".into()]),
        ("rust-fast", helper.clone(), vec!["fast".into(), wrapper.to_string_lossy().into_owned()]),
        ("raw-seqpacket", helper.clone(), vec!["socketpair".into()]),
        ("negative-inet", helper.clone(), vec!["socketpair-inet".into()]),
        ("negative-stream", helper.clone(), vec!["socketpair-stream".into()]),
        ("negative-no-cloexec", helper.clone(), vec!["socketpair-no-cloexec".into()]),
        ("negative-nonblock", helper.clone(), vec!["socketpair-nonblock".into()]),
        ("negative-protocol", helper.clone(), vec!["socketpair-protocol".into()]),
        ("rust-preexec", helper.clone(), vec!["preexec".into(), wrapper.to_string_lossy().into_owned()]),
        ("rust-preexec-denied", helper.clone(), vec!["preexec-denied".into(), "/bin/echo".into()]),
        ("actual-cargo", cargo, vec!["check".into(), "--offline".into(), "--frozen".into(), "--manifest-path".into(), work.join("project/Cargo.toml").to_string_lossy().into_owned()]),
    ];
    for (label, program, args) in modes {
        let responses = |path: &Path| fs::read_dir(path).unwrap().filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "resp")).count();
        let before = responses(session.responses());
        let mut run_environment = environment.clone();
        if label != "actual-cargo" { run_environment.remove("LD_DEBUG"); }
        let output = session.run(&program, &args, &work, &run_environment, deadline, 512 * 1024, &CancellationToken::new()).await.unwrap();
        let after = responses(session.responses());
        eprintln!("P6_RUST_SPAWN_MODE {label} status={:?} er_responses={} stdout={} stderr={}",
            output.status.code(), after-before, String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        if label == "direct-wrapper" { assert!(output.status.success()); }
        if label == "rust-fast" || label == "rust-preexec" {
            assert!(output.status.success());
            assert!(String::from_utf8_lossy(&output.stdout).contains("spawn_exit=Some(0)"));
            assert!(after > before);
        }
        if label == "raw-seqpacket" {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("result=0 errno=0"));
            assert!(stdout.contains("paired_recv flags=0 result=8 errno=0"));
            assert!(stdout.contains("paired_recvfrom flags=0 source=NULL source_len=NULL result=8 errno=0"));
            assert!(stdout.contains("paired_read result=8 packet=[0, 0, 0, 2, 78, 79, 69, 88]"));
            assert!(stdout.contains("child_status=0"));
            for label in ["flags-peek", "nonnull-source", "nonnull-length", "nonnull-both"] {
                assert!(stdout.lines().any(|line| line.contains(&format!("negative_recvfrom {label} ")) && line.contains("result=-1 errno=1")));
            }
            assert_eq!(after, before);
        }
        if label.starts_with("negative-") { assert!(String::from_utf8_lossy(&output.stdout).contains("result=-1 errno=1")); assert_eq!(after, before); }
        if label == "rust-preexec-denied" { assert!(output.status.success()); assert!(String::from_utf8_lossy(&output.stdout).contains("spawn_errno=Some(13)")); }
        if label == "actual-cargo" { assert!(output.status.success(), "offline Cargo check failed: {}", String::from_utf8_lossy(&output.stderr)); assert!(after > before); }
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn p6_native_exec_gate_posix_spawn_wrapper_registers_fixture_image() {
    let mut fixture = Fixture::new();
    let components = fixture.exec_components();
    let alternate = fixture.worker.with_file_name("worker-b");
    compile_fixture_image(&fixture.worker.with_file_name("worker.c"), &alternate, 2);
    assert_ne!(fs::read(&alternate).unwrap(), fs::read(&fixture.worker).unwrap());
    fixture.config.fixed_programs.push(alternate.clone());
    fixture.config.sandbox.allow_read.push(alternate.clone());
    let mut session = NativeBuildSession::start(fixture.config.clone()).await.unwrap();
    let unknown = session.exec_pool().join("not-registered-by-this-session");
    fs::copy(&fixture.worker, &unknown).unwrap();
    fs::set_permissions(&unknown, fs::Permissions::from_mode(0o555)).unwrap();
    let denied = session.run(&fixture.worker, &["deny-program".into(), unknown.to_string_lossy().into_owned()],
        &fixture.config.generated_roots[0], &components.environment, Instant::now()+Duration::from_secs(5), 8192, &CancellationToken::new()).await.unwrap();
    assert!(denied.status.success(), "unknown pool execution was not refused: {}", String::from_utf8_lossy(&denied.stderr));
    assert_eq!(denied.stdout, b"unknown-pool-denied\n");
    fs::remove_file(unknown).unwrap();
    let arguments = vec!["gate-compile".into(), components.wrapper.to_string_lossy().into_owned(),
        components.generated.to_string_lossy().into_owned(), components.leaf_marker.to_string_lossy().into_owned()];
    let output = session.run(&fixture.worker, &arguments, &fixture.config.generated_roots[0], &components.environment,
        Instant::now()+Duration::from_secs(5), 8192, &CancellationToken::new()).await.unwrap();
    if !output.status.success() {
        for entry in fs::read_dir(session.responses()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|value| value == "resp") {
                eprintln!("component gate response {:?}: {:?}", path.file_name(), fs::read(&path).unwrap());
            }
        }
        eprintln!("component pending requests: {:?}", fs::read_dir(session.requests()).unwrap().map(|item| item.unwrap().file_name()).collect::<Vec<_>>());
    }
    assert!(output.status.success(), "status {:?}, stdout {}, stderr {}", output.status,
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("component-posix-spawn"));
    let responses = fixture.assert_exec_component(&session, &components);
    let mut wrapper_reply = vec![0]; wrapper_reply.extend_from_slice(components.wrapper.as_os_str().as_encoded_bytes());
    assert!(responses.contains(&wrapper_reply), "the real posix_spawn interposition must emit E for the wrapper");
    let mut sealed_reply = vec![0]; sealed_reply.extend_from_slice(components.generated.canonicalize().unwrap().as_os_str().as_encoded_bytes());
    assert!(responses.iter().filter(|reply| **reply == sealed_reply).count() >= 2,
        "both R and the subsequent exec gate must observe the sealed fixture image");
    let first_sealed = components.generated.canonicalize().unwrap();
    use std::os::unix::fs::MetadataExt;
    let first_identity = (fs::metadata(&first_sealed).unwrap().dev(), fs::metadata(&first_sealed).unwrap().ino());
    for (image, variant) in [(&alternate, 2), (&fixture.worker, 1)] {
        // Leave the old output alias in place. The real compiler wrapper must
        // detach its own session's alias before the fixture producer writes.
        fs::remove_file(&components.compiler_marker).unwrap();
        fs::remove_file(&components.leaf_marker).unwrap();
        let mut environment = components.environment.clone();
        environment.insert("PAKRO_NATIVE_CC_ARGS".into(), ["fake-compiler".to_owned(), image.to_string_lossy().into_owned(), components.compiler_marker.to_string_lossy().into_owned()].join("\u{1f}"));
        let repeated = session.run(&fixture.worker, &arguments, &fixture.config.generated_roots[0], &environment,
            Instant::now()+Duration::from_secs(5), 8192, &CancellationToken::new()).await.unwrap();
        assert!(repeated.status.success(), "A/B/A fixture producer failed: {}", String::from_utf8_lossy(&repeated.stderr));
        assert!(String::from_utf8_lossy(&repeated.stdout).contains(&format!("component-variant={variant}\n")));
        let sealed = components.generated.canonicalize().unwrap();
        assert!(sealed.starts_with(session.exec_pool()));
        assert_eq!(fs::read(&sealed).unwrap(), fs::read(image).unwrap());
        assert_eq!(fs::metadata(&sealed).unwrap().mode() & 0o222, 0);
        if variant == 1 {
            assert_eq!(sealed, first_sealed, "returning to A must reuse its original sealed digest");
            assert_eq!((fs::metadata(&sealed).unwrap().dev(), fs::metadata(&sealed).unwrap().ino()), first_identity,
                "A/B/A must preserve the original immutable A object");
        } else { assert_ne!(sealed, first_sealed); }
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn p6_native_shell_boundary_wrapper_restores_gate_and_seals_fixture_image() {
    let mut fixture = Fixture::new();
    let components = fixture.exec_components();
    let chain = native_shell_inputs().unwrap();
    let shell = chain.programs[0].clone();
    fixture.config.fixed_programs.extend(chain.programs);
    fixture.config.sandbox.allow_read.extend(chain.read_inputs);
    let mut session = NativeBuildSession::start(fixture.config.clone()).await.unwrap();
    // SIP strips these on macOS; explicitly removing them makes the same
    // wrapper restoration requirement observable on Linux too.
    let arguments = vec!["-c".into(),
        ": >/dev/null || exit 60; [ -d / ] || exit 61; unset DYLD_INSERT_LIBRARIES LD_PRELOAD; \"$1\" -o \"$2\" && \"$2\" component-leaf \"$3\" && printf 'component-sip-shell\\n'".into(),
        "component-shell".into(), components.wrapper.to_string_lossy().into_owned(),
        components.generated.to_string_lossy().into_owned(), components.leaf_marker.to_string_lossy().into_owned()];
    let output = session.run(&shell, &arguments, &fixture.config.generated_roots[0], &components.environment,
        Instant::now()+Duration::from_secs(5), 8192, &CancellationToken::new()).await.unwrap();
    assert!(output.status.success(), "status {:?}, stdout {}, stderr {}", output.status,
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("component-sip-shell"));
    let responses = fixture.assert_exec_component(&session, &components);
    let mut compiler_reply = vec![0]; compiler_reply.extend_from_slice(fixture.worker.as_os_str().as_encoded_bytes());
    assert!(responses.iter().filter(|reply| **reply == compiler_reply).count() >= 2,
        "the restored gate must service the fake compiler's real posix_spawn, in addition to the wrapper's E");
    session.close().await.unwrap();
}

#[tokio::test]
async fn p6_native_session_keeps_hooks_and_cleans_normal_job_groups() {
    let fixture = Fixture::new();
    let mut session = NativeBuildSession::start(fixture.config.clone())
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let args = vec![
        "normal".into(),
        fixture.tick_path.to_string_lossy().into_owned(),
    ];
    let output = session
        .run(
            &fixture.worker,
            &args,
            &fixture.config.generated_roots[0],
            &BTreeMap::new(),
            Instant::now() + Duration::from_secs(5),
            4096,
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("done"));
    assert!(fixture.ticks.metadata().unwrap().len() > 0);
    fixture.stopped(false).await;
    let output = session
        .run(
            &fixture.worker,
            &["leaf".into()],
            &fixture.config.generated_roots[0],
            &BTreeMap::new(),
            Instant::now() + Duration::from_secs(5),
            4096,
            &cancel,
        )
        .await
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"leaf\n");
    let environment = fixture.config.workspace.join("new-buildenv");
    fs::create_dir(&environment).unwrap();
    let extra = environment.join("bin");
    fs::copy(&fixture.worker, &extra).unwrap();
    use std::os::unix::fs::MetadataExt;
    let inode = fs::metadata(&extra).unwrap().ino();
    let sha256 = session
        .program_identities()
        .iter()
        .find(|i| i.path == fixture.worker)
        .unwrap()
        .sha256;
    session
        .register_fixed_inputs(&[NativeProgramIdentity {
            path: extra.clone(),
            sha256,
        }])
        .await
        .unwrap();
    let output = session
        .run(
            &extra,
            &["leaf".into()],
            &fixture.config.generated_roots[0],
            &BTreeMap::new(),
            Instant::now() + Duration::from_secs(5),
            4096,
            &cancel,
        )
        .await
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"leaf\n");
    assert_eq!(
        fs::metadata(extra).unwrap().ino(),
        inode,
        "fixed buildenv registration changed its inode"
    );
    session.close().await.unwrap();
    assert!(
        fixture.config.workspace.exists(),
        "normal close must preserve source/output ownership"
    );
}

#[tokio::test]
async fn p6_native_timeout_cancel_and_output_limit_stop_owned_jobs() {
    for mode in ["timeout", "cancel", "limit"] {
        let fixture = Fixture::new();
        let mut session = NativeBuildSession::start(fixture.config.clone())
            .await
            .unwrap();
        let cancellation = CancellationToken::new();
        let trigger = cancellation.clone();
        let duration = if mode == "timeout" {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(5)
        };
        let deadline = Instant::now() + duration;
        let ticks = (mode == "cancel").then(|| fixture.ticks.try_clone().unwrap());
        let task = tokio::spawn(async move {
            if let Some(ticks) = ticks {
                while ticks.metadata().unwrap().len() == 0 {
                    if Instant::now() >= deadline { return; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                if Instant::now() < deadline { trigger.cancel(); }
            }
        });
        let args = vec![
            if mode == "limit" { "limit" } else { "hang" }.into(),
            fixture.tick_path.to_string_lossy().into_owned(),
        ];
        let error = session
            .run(
                &fixture.worker,
                &args,
                &fixture.config.generated_roots[0],
                &BTreeMap::new(),
                deadline,
                1024,
                &cancellation,
            )
            .await
            .unwrap_err();
        assert!(
            fixture.ticks.metadata().unwrap().len() > 0,
            "{mode}: owned grandchild did not actually run"
        );
        if mode == "limit" {
            assert!(
                format!("{error:#}").contains("output exceeded"),
                "{error:#}"
            );
        } else {
            assert!(
                format!("{error:#}").contains("cancelled")
                    || format!("{error:#}").contains("deadline"),
                "{error:#}"
            );
        }
        task.await.unwrap();
        if mode == "cancel" { assert!(cancellation.is_cancelled(), "the cancel case must actually trigger cancellation after its child runs"); }
        fixture.stopped(true).await;
    }
}

#[tokio::test]
async fn p6_native_generated_registration_and_fs_network_isolation() {
    let fixture = Fixture::new();
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = tcp.local_addr().unwrap().port();
    let socket_path = fixture.secret.with_file_name("outside.sock");
    let unix = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    // The peers are actually reachable from the unsandboxed test owner.
    std::net::TcpStream::connect(tcp.local_addr().unwrap()).unwrap();
    std::os::unix::net::UnixStream::connect(&socket_path).unwrap();
    let mut session = NativeBuildSession::start(fixture.config.clone())
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    let args = vec![
        "isolation".into(),
        fixture.secret.to_string_lossy().into_owned(),
        fixture.outside.to_string_lossy().into_owned(),
        port.to_string(),
        socket_path.to_string_lossy().into_owned(),
    ];
    let output = session
        .run(
            &fixture.worker,
            &args,
            &fixture.config.generated_roots[0],
            &BTreeMap::new(),
            Instant::now() + Duration::from_secs(5),
            4096,
            &cancellation,
        )
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "status {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"isolated\n");
    assert!(!fixture.outside.exists());
    assert_eq!(fs::read(&fixture.secret).unwrap(), b"private outside bytes");
    let generated = fixture.config.generated_roots[0].join("generated");
    let args = vec![
        "register".into(),
        fixture.worker.to_string_lossy().into_owned(),
        generated.to_string_lossy().into_owned(),
    ];
    let output = session
        .run(
            &fixture.worker,
            &args,
            &fixture.config.generated_roots[0],
            &BTreeMap::new(),
            Instant::now() + Duration::from_secs(10),
            4096,
            &cancellation,
        )
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "status {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"leaf\nregistered\n");
    let sealed = generated.canonicalize().unwrap();
    assert!(sealed.starts_with(session.exec_pool()));
    assert_eq!(
        fs::read(&sealed).unwrap(),
        fs::read(&fixture.worker).unwrap()
    );
    assert_eq!(fs::read_dir(session.requests()).unwrap().count(), 0);
    session.close().await.unwrap();
}

#[tokio::test]
async fn p6_native_owner_sigkill_stops_grandchild_and_cleans_workspace() {
    let fixture = Fixture::new();
    let configuration = fixture._root.path().join("fixture.json");
    fs::write(&configuration, serde_json::to_vec(&fixture.config).unwrap()).unwrap();
    let mut owner = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native_build::tests::p6_native_owner_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("PAKRO_NATIVE_OWNER_HELPER_CONFIG", configuration)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while fixture.ticks.metadata().unwrap().len() == 0 && Instant::now() < until {
        assert!(
            owner.try_wait().unwrap().is_none(),
            "owner helper exited before its child ran"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        fixture.ticks.metadata().unwrap().len() > 0,
        "owned grandchild never started"
    );
    owner.kill().unwrap();
    let status = owner.wait().unwrap();
    assert_eq!(status.signal(), Some(9));
    fixture.stopped(true).await;
}

#[tokio::test]
#[ignore = "only the owner-SIGKILL parent starts this exact helper"]
async fn p6_native_owner_helper() {
    let path = std::env::var_os("PAKRO_NATIVE_OWNER_HELPER_CONFIG")
        .expect("dedicated owner fixture is required");
    let config: NativeBuildConfig = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let worker = config.fixed_programs[0].clone();
    let work = config.generated_roots[0].clone();
    let args = vec![
        "hang".into(),
        work.join("ticks").to_string_lossy().into_owned(),
    ];
    let mut session = NativeBuildSession::start(config).await.unwrap();
    session
        .run(
            &worker,
            &args,
            &work,
            &BTreeMap::new(),
            Instant::now() + Duration::from_secs(20),
            4096,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    panic!("owner fixture must be killed by its parent");
}
