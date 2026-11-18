;; Copyright 2026 Ast Rin
;; SPDX-License-Identifier: Apache-2.0

;; 面向 lancet-act 的本地 WASI 适配组件。
;;
;; 仅提供 Rust 标准库为 wasm32-wasip2 引入的 WASI 接口：
;; - 环境变量为空；
;; - 时钟返回固定值；
;; - 非安全随机种子为固定值；
;; - 终端查询返回 none；
;; - 其余操作（标准流读写、exit、pollable.block）一律 unreachable 陷入。

(component
  ;; 不带线性内存的陷入与固定返回核心模块。
  (core module $m_trap
    (func (export "dtor") (param i32))
    (func (export "monotonic_now") (result i64)
      (i64.const 1)
    )
    (func (export "trap_1_0") (param i32)
      (unreachable)
    )
    (func (export "trap_0_1") (result i32)
      (unreachable)
    )
    (func (export "trap_1_1") (param i32) (result i32)
      (unreachable)
    )
  )

  ;; 带 1 页零初始化线性内存的核心模块，用于需要返回内存切片或复合值的规范提升函数。
  (core module $m_mem
    (memory (export "mem") 1)
    ;; 内存布局：
    ;;   0..8:   空 list / option::none（全零）
    ;;   8..24:  wall-clock datetime { seconds: 1700000000, nanoseconds: 0 }
    ;;   24..40: insecure-seed (0x0123456789abcdef, 0xfedcba9876543210)
    (data (i32.const 8) "\00\04\55\65\00\00\00\00")
    (data (i32.const 24) "\ef\cd\ab\89\67\45\23\01\10\32\54\76\98\ba\dc\fe")

    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (unreachable)
    )
    (func (export "dtor") (param i32))
    (func (export "ret_0") (result i32)
      (i32.const 0)
    )
    (func (export "ret_8") (result i32)
      (i32.const 8)
    )
    (func (export "ret_24") (result i32)
      (i32.const 24)
    )
    (func (export "trap_1_1") (param i32) (result i32)
      (unreachable)
    )
    (func (export "trap_3_1") (param i32 i32 i32) (result i32)
      (unreachable)
    )
  )

  ;; 1. wasi:io/poll@0.2.6
  (component $c_poll
    (core instance $ci (instantiate $m_trap))
    (alias core export $ci "dtor" (core func $dtor))
    (alias core export $ci "trap_1_0" (core func $c_block))
    (type $pollable_r (resource (rep i32) (dtor (func $dtor))))
    (export $pollable "pollable" (type $pollable_r))
    (type $fn_block (func (param "self" (borrow $pollable))))
    (func $f_block (type $fn_block) (canon lift (core func $c_block)))
    (export "[method]pollable.block" (func $f_block))
  )
  (instance $poll_inst (instantiate $c_poll))
  (export "wasi:io/poll@0.2.6" (instance $poll_inst))

  ;; 2. wasi:io/error@0.2.6
  (component $c_error
    (core instance $ci (instantiate $m_trap))
    (alias core export $ci "dtor" (core func $dtor))
    (type $error_r (resource (rep i32) (dtor (func $dtor))))
    (export "error" (type $error_r))
  )
  (instance $error_inst (instantiate $c_error))
  (export "wasi:io/error@0.2.6" (instance $error_inst))

  ;; 3. wasi:io/streams@0.2.6
  (component $c_streams
    (import "error" (type $in_error (sub resource)))
    (import "pollable" (type $in_pollable (sub resource)))
    (core instance $ci (instantiate $m_mem))
    (alias core export $ci "mem" (core memory $mem))
    (alias core export $ci "realloc" (core func $realloc))
    (alias core export $ci "dtor" (core func $dtor))
    (alias core export $ci "trap_1_1" (core func $trap_1_1))
    (alias core export $ci "trap_3_1" (core func $trap_3_1))

    (type $in_stream_r (resource (rep i32) (dtor (func $dtor))))
    (type $out_stream_r (resource (rep i32) (dtor (func $dtor))))
    (export "input-stream" (type $in_stream_r))
    (export $out_stream "output-stream" (type $out_stream_r))
    (export $error "error" (type $in_error))
    (type $stream_error (variant (case "last-operation-failed" (own $error)) (case "closed")))
    (export $e_stream_error "stream-error" (type $stream_error))
    (export $pollable "pollable" (type $in_pollable))

    (type $res_check_write (result u64 (error $e_stream_error)))
    (type $fn_check_write (func (param "self" (borrow $out_stream)) (result $res_check_write)))
    (type $list_u8 (list u8))
    (type $res_unit_stream (result (error $e_stream_error)))
    (type $fn_write (func (param "self" (borrow $out_stream)) (param "contents" $list_u8) (result $res_unit_stream)))
    (type $fn_blocking_flush (func (param "self" (borrow $out_stream)) (result $res_unit_stream)))
    (type $fn_subscribe (func (param "self" (borrow $out_stream)) (result (own $pollable))))

    (func $f_check_write (type $fn_check_write) (canon lift (core func $trap_1_1) (memory $mem)))
    (func $f_write (type $fn_write) (canon lift (core func $trap_3_1) (memory $mem) (realloc $realloc)))
    (func $f_blocking_flush (type $fn_blocking_flush) (canon lift (core func $trap_1_1) (memory $mem)))
    (func $f_subscribe (type $fn_subscribe) (canon lift (core func $trap_1_1)))

    (export "[method]output-stream.check-write" (func $f_check_write))
    (export "[method]output-stream.write" (func $f_write))
    (export "[method]output-stream.blocking-flush" (func $f_blocking_flush))
    (export "[method]output-stream.subscribe" (func $f_subscribe))
  )
  (alias export $error_inst "error" (type $t_error))
  (alias export $poll_inst "pollable" (type $t_pollable))
  (instance $streams_inst (instantiate $c_streams (with "error" (type $t_error)) (with "pollable" (type $t_pollable))))
  (export "wasi:io/streams@0.2.6" (instance $streams_inst))

  ;; 4. wasi:clocks/monotonic-clock@0.2.6
  (component $c_mono
    (core instance $ci (instantiate $m_trap))
    (alias core export $ci "monotonic_now" (core func $c_now))
    (type $instant u64)
    (export $e_instant "instant" (type $instant))
    (type $fn_now (func (result $e_instant)))
    (func $f_now (type $fn_now) (canon lift (core func $c_now)))
    (export "now" (func $f_now))
  )
  (instance $mono_inst (instantiate $c_mono))
  (export "wasi:clocks/monotonic-clock@0.2.6" (instance $mono_inst))

  ;; 5. wasi:clocks/wall-clock@0.2.6
  (component $c_wall
    (core instance $ci (instantiate $m_mem))
    (alias core export $ci "mem" (core memory $mem))
    (alias core export $ci "ret_8" (core func $c_now))
    (type $datetime (record (field "seconds" u64) (field "nanoseconds" u32)))
    (export $e_datetime "datetime" (type $datetime))
    (type $fn_now (func (result $e_datetime)))
    (func $f_now (type $fn_now) (canon lift (core func $c_now) (memory $mem)))
    (export "now" (func $f_now))
  )
  (instance $wall_inst (instantiate $c_wall))
  (export "wasi:clocks/wall-clock@0.2.6" (instance $wall_inst))

  ;; 6. wasi:cli/environment@0.2.6
  (component $c_env
    (core instance $ci (instantiate $m_mem))
    (alias core export $ci "mem" (core memory $mem))
    (alias core export $ci "ret_0" (core func $c_get_env))
    (type $pair (tuple string string))
    (type $env_list (list $pair))
    (type $fn_get_env (func (result $env_list)))
    (func $f_get_env (type $fn_get_env) (canon lift (core func $c_get_env) (memory $mem) string-encoding=utf8))
    (export "get-environment" (func $f_get_env))
  )
  (instance $env_inst (instantiate $c_env))
  (export "wasi:cli/environment@0.2.6" (instance $env_inst))

  ;; 7. wasi:cli/exit@0.2.6
  (component $c_exit
    (core instance $ci (instantiate $m_trap))
    (alias core export $ci "trap_1_0" (core func $c_exit))
    (type $res (result))
    (type $fn_exit (func (param "status" $res)))
    (func $f_exit (type $fn_exit) (canon lift (core func $c_exit)))
    (export "exit" (func $f_exit))
  )
  (instance $exit_inst (instantiate $c_exit))
  (export "wasi:cli/exit@0.2.6" (instance $exit_inst))

  ;; 8. wasi:cli/stdin@0.2.6
  (component $c_stdin
    (import "input-stream" (type $in_s (sub resource)))
    (core instance $ci (instantiate $m_trap))
    (alias core export $ci "trap_0_1" (core func $c_get))
    (export $e_in "input-stream" (type $in_s))
    (type $fn_get (func (result (own $e_in))))
    (func $f_get (type $fn_get) (canon lift (core func $c_get)))
    (export "get-stdin" (func $f_get))
  )
  (alias export $streams_inst "input-stream" (type $t_in_stream))
  (instance $stdin_inst (instantiate $c_stdin (with "input-stream" (type $t_in_stream))))
  (export "wasi:cli/stdin@0.2.6" (instance $stdin_inst))

  ;; 9-10. wasi:cli/stdout@0.2.6 与 stderr@0.2.6
  (component $c_stdout
    (import "output-stream" (type $out_s (sub resource)))
    (core instance $ci (instantiate $m_trap))
    (alias core export $ci "trap_0_1" (core func $c_get))
    (export $e_out "output-stream" (type $out_s))
    (type $fn_get (func (result (own $e_out))))
    (func $f_get (type $fn_get) (canon lift (core func $c_get)))
    (export "get-stdout" (func $f_get))
  )
  (component $c_stderr
    (import "output-stream" (type $out_s (sub resource)))
    (core instance $ci (instantiate $m_trap))
    (alias core export $ci "trap_0_1" (core func $c_get))
    (export $e_out "output-stream" (type $out_s))
    (type $fn_get (func (result (own $e_out))))
    (func $f_get (type $fn_get) (canon lift (core func $c_get)))
    (export "get-stderr" (func $f_get))
  )
  (alias export $streams_inst "output-stream" (type $t_out_stream))
  (instance $stdout_inst (instantiate $c_stdout (with "output-stream" (type $t_out_stream))))
  (export "wasi:cli/stdout@0.2.6" (instance $stdout_inst))
  (instance $stderr_inst (instantiate $c_stderr (with "output-stream" (type $t_out_stream))))
  (export "wasi:cli/stderr@0.2.6" (instance $stderr_inst))

  ;; 11-12. wasi:cli/terminal-input 与 terminal-output@0.2.6
  (component $c_term_in
    (core instance $ci (instantiate $m_trap))
    (alias core export $ci "dtor" (core func $dtor))
    (type $ti (resource (rep i32) (dtor (func $dtor))))
    (export "terminal-input" (type $ti))
  )
  (component $c_term_out
    (core instance $ci (instantiate $m_trap))
    (alias core export $ci "dtor" (core func $dtor))
    (type $to (resource (rep i32) (dtor (func $dtor))))
    (export "terminal-output" (type $to))
  )
  (instance $term_in_inst (instantiate $c_term_in))
  (export "wasi:cli/terminal-input@0.2.6" (instance $term_in_inst))
  (instance $term_out_inst (instantiate $c_term_out))
  (export "wasi:cli/terminal-output@0.2.6" (instance $term_out_inst))

  ;; 13-15. wasi:cli/terminal-stdin、terminal-stdout、terminal-stderr@0.2.6
  (component $c_term_stdin
    (import "terminal-input" (type $ti (sub resource)))
    (core instance $ci (instantiate $m_mem))
    (alias core export $ci "mem" (core memory $mem))
    (alias core export $ci "ret_0" (core func $c_get))
    (export $e_ti "terminal-input" (type $ti))
    (type $opt_ti (option (own $e_ti)))
    (type $fn_get (func (result $opt_ti)))
    (func $f_get (type $fn_get) (canon lift (core func $c_get) (memory $mem)))
    (export "get-terminal-stdin" (func $f_get))
  )
  (component $c_term_stdout
    (import "terminal-output" (type $to (sub resource)))
    (core instance $ci (instantiate $m_mem))
    (alias core export $ci "mem" (core memory $mem))
    (alias core export $ci "ret_0" (core func $c_get))
    (export $e_to "terminal-output" (type $to))
    (type $opt_to (option (own $e_to)))
    (type $fn_get (func (result $opt_to)))
    (func $f_get (type $fn_get) (canon lift (core func $c_get) (memory $mem)))
    (export "get-terminal-stdout" (func $f_get))
  )
  (component $c_term_stderr
    (import "terminal-output" (type $to (sub resource)))
    (core instance $ci (instantiate $m_mem))
    (alias core export $ci "mem" (core memory $mem))
    (alias core export $ci "ret_0" (core func $c_get))
    (export $e_to "terminal-output" (type $to))
    (type $opt_to (option (own $e_to)))
    (type $fn_get (func (result $opt_to)))
    (func $f_get (type $fn_get) (canon lift (core func $c_get) (memory $mem)))
    (export "get-terminal-stderr" (func $f_get))
  )
  (alias export $term_in_inst "terminal-input" (type $t_ti))
  (alias export $term_out_inst "terminal-output" (type $t_to))
  (instance $term_stdin_inst (instantiate $c_term_stdin (with "terminal-input" (type $t_ti))))
  (export "wasi:cli/terminal-stdin@0.2.6" (instance $term_stdin_inst))
  (instance $term_stdout_inst (instantiate $c_term_stdout (with "terminal-output" (type $t_to))))
  (export "wasi:cli/terminal-stdout@0.2.6" (instance $term_stdout_inst))
  (instance $term_stderr_inst (instantiate $c_term_stderr (with "terminal-output" (type $t_to))))
  (export "wasi:cli/terminal-stderr@0.2.6" (instance $term_stderr_inst))

  ;; 16. wasi:random/insecure-seed@0.2.6
  (component $c_seed
    (core instance $ci (instantiate $m_mem))
    (alias core export $ci "mem" (core memory $mem))
    (alias core export $ci "ret_24" (core func $c_seed))
    (type $seed_t (tuple u64 u64))
    (type $fn_seed (func (result $seed_t)))
    (func $f_seed (type $fn_seed) (canon lift (core func $c_seed) (memory $mem)))
    (export "insecure-seed" (func $f_seed))
  )
  (instance $seed_inst (instantiate $c_seed))
  (export "wasi:random/insecure-seed@0.2.6" (instance $seed_inst))
)