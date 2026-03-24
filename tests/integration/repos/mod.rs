#[macro_use]
pub mod test_file;
pub mod test_repo;

#[macro_export]
macro_rules! subdir_test_variants {
    (
        fn $test_name:ident() $body:block
    ) => {
        paste::paste! {
            // Variant 1: Run from subdirectory (original behavior)
            #[test]
            fn [<test_ $test_name _from_subdir>]() $body

            // Variant 1b: Run from subdirectory with a worktree-backed repo
            #[test]
            fn [<test_ $test_name _from_subdir_in_worktree>]() {
                $crate::repos::test_repo::with_worktree_mode(|| {
                    [<test_ $test_name _from_subdir>]();
                });
            }

            // Variant 2: Run with -C flag from arbitrary directory
            #[test]
            fn [<test_ $test_name _with_c_flag>]() {
                // Wrapper struct that intercepts git calls to use -C flag
                struct TestRepoWithCFlag {
                    inner: $crate::repos::test_repo::TestRepo,
                }

                #[allow(dead_code)]
                impl TestRepoWithCFlag {
                    fn new() -> Self {
                        Self { inner: $crate::repos::test_repo::TestRepo::new() }
                    }

                    fn git_from_working_dir(
                        &self,
                        _working_dir: &std::path::Path,
                        args: &[&str],
                    ) -> Result<String, String> {
                        // Prepend -C <repo_root> to args and run from arbitrary directory
                        let arbitrary_dir = std::env::temp_dir();

                        use std::process::Command;
                        use $crate::repos::test_repo::{
                            get_binary_path,
                            git_command_requires_daemon_sync, git_command_routes_to_clone_target,
                            new_daemon_test_sync_session_id, GitTestMode,
                        };

                        let binary_path = get_binary_path();
                        let mode = GitTestMode::from_env();
                        let command_affects_daemon = self
                            .inner
                            .git_command_affects_daemon_for_tracking(
                                args,
                                Some(self.inner.path().as_path()),
                            );

                        if mode.uses_daemon() && git_command_requires_daemon_sync(args) {
                            self.inner.sync_daemon_force();
                        }

                        let daemon_command_pending = mode.uses_daemon()
                            && command_affects_daemon
                            && !git_command_routes_to_clone_target(args);
                        let daemon_test_sync_session =
                            daemon_command_pending.then(new_daemon_test_sync_session_id);
                        let mut full_args = vec![
                            "-C".to_string(),
                            self.inner.path().to_str().unwrap().to_string(),
                        ];
                        if let Some(session) = daemon_test_sync_session.as_deref() {
                            self.inner
                                .append_daemon_test_sync_session_args(&mut full_args, session);
                        }
                        full_args.extend(args.iter().map(|arg| (*arg).to_string()));

                        let mut command = if mode.uses_wrapper() {
                            Command::new(binary_path)
                        } else {
                            Command::new($crate::repos::test_repo::real_git_executable())
                        };
                        command.current_dir(&arbitrary_dir);
                        command.args(&full_args);
                        self.inner.configure_test_git_command_env(&mut command);

                        let output = command.output().expect(&format!(
                            "Failed to execute git command with -C flag: {:?}", args
                        ));

                        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                        if output.status.success() {
                            if daemon_command_pending {
                                self.inner
                                    .record_daemon_family_expected_completion_session(
                                        daemon_test_sync_session
                                            .as_deref()
                                            .expect("daemon test sync session should exist"),
                                    );
                            }
                            Ok(if stdout.is_empty() { stderr } else { stdout })
                        } else {
                            if daemon_command_pending {
                                self.inner
                                    .record_daemon_family_expected_completion_session(
                                        daemon_test_sync_session
                                            .as_deref()
                                            .expect("daemon test sync session should exist"),
                                    );
                            }
                            Err(stderr)
                        }
                    }

                    fn git_with_env(
                        &self,
                        args: &[&str],
                        envs: &[(&str, &str)],
                        working_dir: Option<&std::path::Path>,
                    ) -> Result<String, String> {
                        if working_dir.is_some() {
                            // If working_dir is specified, prepend -C and run from arbitrary dir
                            let arbitrary_dir = std::env::temp_dir();

                            use std::process::Command;
                            use $crate::repos::test_repo::{
                                get_binary_path,
                                git_command_requires_daemon_sync,
                                git_command_routes_to_clone_target,
                                new_daemon_test_sync_session_id, GitTestMode,
                            };

                        let binary_path = get_binary_path();
                        let mode = GitTestMode::from_env();
                        let command_affects_daemon = self
                            .inner
                            .git_command_affects_daemon_for_tracking(
                                args,
                                Some(self.inner.path().as_path()),
                            );

                        if mode.uses_daemon() && git_command_requires_daemon_sync(args) {
                            self.inner.sync_daemon_force();
                        }

                        let daemon_command_pending = mode.uses_daemon()
                            && command_affects_daemon
                            && !git_command_routes_to_clone_target(args);
                        let daemon_test_sync_session =
                            daemon_command_pending.then(new_daemon_test_sync_session_id);
                        let mut full_args = vec![
                            "-C".to_string(),
                            self.inner.path().to_str().unwrap().to_string(),
                        ];
                        if let Some(session) = daemon_test_sync_session.as_deref() {
                            self.inner
                                .append_daemon_test_sync_session_args(&mut full_args, session);
                        }
                        full_args.extend(args.iter().map(|arg| (*arg).to_string()));

                            let mut command = if mode.uses_wrapper() {
                                Command::new(binary_path)
                            } else {
                                Command::new($crate::repos::test_repo::real_git_executable())
                            };
                            command.current_dir(&arbitrary_dir);
                            command.args(&full_args);
                            self.inner.configure_test_git_command_env(&mut command);

                            // Apply custom env vars
                            for (key, value) in envs {
                                command.env(key, value);
                            }

                            let output = command.output().expect(&format!(
                                "Failed to execute git command with -C flag and env: {:?}", args
                            ));

                            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                            if output.status.success() {
                                if daemon_command_pending {
                                    self.inner.record_daemon_family_expected_completion_session(
                                        daemon_test_sync_session
                                            .as_deref()
                                            .expect("daemon test sync session should exist"),
                                    );
                                }
                                Ok(if stdout.is_empty() { stderr } else { stdout })
                            } else {
                                if daemon_command_pending {
                                    self.inner.record_daemon_family_expected_completion_session(
                                        daemon_test_sync_session
                                            .as_deref()
                                            .expect("daemon test sync session should exist"),
                                    );
                                }
                                Err(stderr)
                            }
                        } else {
                            // No working_dir, use normal behavior
                            self.inner.git_with_env(args, envs, None)
                        }
                    }
                }

                // Forward all other methods via Deref
                impl std::ops::Deref for TestRepoWithCFlag {
                    type Target = $crate::repos::test_repo::TestRepo;
                    fn deref(&self) -> &Self::Target {
                        &self.inner
                    }
                }

                // Type alias to shadow TestRepo
                type TestRepo = TestRepoWithCFlag;
                $body
            }

            // Variant 2b: Run with -C flag from arbitrary directory in worktree mode
            #[test]
            fn [<test_ $test_name _with_c_flag_in_worktree>]() {
                $crate::repos::test_repo::with_worktree_mode(|| {
                    [<test_ $test_name _with_c_flag>]();
                });
            }
        }
    };
}

#[macro_export]
macro_rules! worktree_test_wrappers {
    (
        fn $test_name:ident() $body:block
    ) => {
        paste::paste! {
            #[test]
            fn [<test_ $test_name _in_worktree_wrapper_mode>]() {
                struct WorktreeTestRepo {
                    inner: $crate::repos::test_repo::TestRepo,
                }

                #[allow(dead_code)]
                impl WorktreeTestRepo {
                    fn new() -> Self {
                        Self {
                            inner: $crate::repos::test_repo::TestRepo::new_worktree_with_mode(
                                $crate::repos::test_repo::GitTestMode::Wrapper,
                            ),
                        }
                    }

                    fn new_with_remote() -> (Self, Self) {
                        let (local, upstream) =
                            $crate::repos::test_repo::TestRepo::new_with_remote_with_mode(
                                $crate::repos::test_repo::GitTestMode::Wrapper,
                            );
                        (
                            Self { inner: local },
                            Self { inner: upstream },
                        )
                    }

                    fn git_mode() -> $crate::repos::test_repo::GitTestMode {
                        $crate::repos::test_repo::GitTestMode::Wrapper
                    }
                }

                impl std::ops::Deref for WorktreeTestRepo {
                    type Target = $crate::repos::test_repo::TestRepo;
                    fn deref(&self) -> &Self::Target {
                        &self.inner
                    }
                }

                type TestRepo = WorktreeTestRepo;
                $body
            }

            #[test]
            fn [<test_ $test_name _in_worktree_hooks_mode>]() {
                struct WorktreeTestRepo {
                    inner: $crate::repos::test_repo::TestRepo,
                }

                #[allow(dead_code)]
                impl WorktreeTestRepo {
                    fn new() -> Self {
                        Self {
                            inner: $crate::repos::test_repo::TestRepo::new_worktree_with_mode(
                                $crate::repos::test_repo::GitTestMode::Hooks,
                            ),
                        }
                    }

                    fn new_with_remote() -> (Self, Self) {
                        let (local, upstream) =
                            $crate::repos::test_repo::TestRepo::new_with_remote_with_mode(
                                $crate::repos::test_repo::GitTestMode::Hooks,
                            );
                        (
                            Self { inner: local },
                            Self { inner: upstream },
                        )
                    }

                    fn git_mode() -> $crate::repos::test_repo::GitTestMode {
                        $crate::repos::test_repo::GitTestMode::Hooks
                    }
                }

                impl std::ops::Deref for WorktreeTestRepo {
                    type Target = $crate::repos::test_repo::TestRepo;
                    fn deref(&self) -> &Self::Target {
                        &self.inner
                    }
                }

                type TestRepo = WorktreeTestRepo;
                $body
            }

            #[test]
            fn [<test_ $test_name _in_worktree_both_mode>]() {
                struct WorktreeTestRepo {
                    inner: $crate::repos::test_repo::TestRepo,
                }

                #[allow(dead_code)]
                impl WorktreeTestRepo {
                    fn new() -> Self {
                        Self {
                            inner: $crate::repos::test_repo::TestRepo::new_worktree_with_mode(
                                $crate::repos::test_repo::GitTestMode::Both,
                            ),
                        }
                    }

                    fn new_with_remote() -> (Self, Self) {
                        let (local, upstream) =
                            $crate::repos::test_repo::TestRepo::new_with_remote_with_mode(
                                $crate::repos::test_repo::GitTestMode::Both,
                            );
                        (
                            Self { inner: local },
                            Self { inner: upstream },
                        )
                    }

                    fn git_mode() -> $crate::repos::test_repo::GitTestMode {
                        $crate::repos::test_repo::GitTestMode::Both
                    }
                }

                impl std::ops::Deref for WorktreeTestRepo {
                    type Target = $crate::repos::test_repo::TestRepo;
                    fn deref(&self) -> &Self::Target {
                        &self.inner
                    }
                }

                type TestRepo = WorktreeTestRepo;
                $body
            }

            #[test]
            fn [<test_ $test_name _in_worktree_daemon_mode>]() {
                struct WorktreeTestRepo {
                    inner: $crate::repos::test_repo::TestRepo,
                }

                #[allow(dead_code)]
                impl WorktreeTestRepo {
                    fn new() -> Self {
                        Self {
                            inner: $crate::repos::test_repo::TestRepo::new_worktree_with_mode(
                                $crate::repos::test_repo::GitTestMode::Daemon,
                            ),
                        }
                    }

                    fn new_with_remote() -> (Self, Self) {
                        let (local, upstream) =
                            $crate::repos::test_repo::TestRepo::new_with_remote_with_mode(
                                $crate::repos::test_repo::GitTestMode::Daemon,
                            );
                        (
                            Self { inner: local },
                            Self { inner: upstream },
                        )
                    }

                    fn git_mode() -> $crate::repos::test_repo::GitTestMode {
                        $crate::repos::test_repo::GitTestMode::Daemon
                    }
                }

                impl std::ops::Deref for WorktreeTestRepo {
                    type Target = $crate::repos::test_repo::TestRepo;
                    fn deref(&self) -> &Self::Target {
                        &self.inner
                    }
                }

                type TestRepo = WorktreeTestRepo;
                $body
            }
        }
    };
}

#[macro_export]
macro_rules! reuse_tests_in_worktree {
    (
        $( $test_name:ident ),+ $(,)?
    ) => {
        paste::paste! {
            $(
                #[test]
                fn [<$test_name _in_worktree>]() {
                    $crate::repos::test_repo::with_worktree_mode(|| {
                        $test_name();
                    })
                }
            )+
        }
    };
}

#[macro_export]
macro_rules! reuse_tests_in_worktree_with_attrs {
    (
        ($($attrs:tt)*)
        $test_name:ident
        $(, $rest:ident)* $(,)?
    ) => {
        $crate::reuse_tests_in_worktree_with_attrs!(@one ($($attrs)*) $test_name);
        $crate::reuse_tests_in_worktree_with_attrs!(($($attrs)*) $($rest),*);
    };
    (
        ($($attrs:tt)*)
    ) => {
    };
    (@one ($($attrs:tt)*) $test_name:ident) => {
        paste::paste! {
            $($attrs)*
            #[test]
            fn [<$test_name _in_worktree>]() {
                $crate::repos::test_repo::with_worktree_mode(|| {
                    $test_name();
                })
            }
        }
    };
}
