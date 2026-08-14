# frozen_string_literal: true

# Test harness for driving the `trongo` CLI as a subprocess.
#
# Every stress test runs the interpreter out-of-process under a wall-clock
# timeout and a hard memory cap. This is the only way to test resource
# metering honestly:
#
#   - A missing recursion guard produces a stack overflow. In-process that
#     is a SIGSEGV, and a crashed test runner reports nothing useful. Out of
#     process it is an observable `Process::Status` with a signal.
#   - A missing allocation guard produces an OOM. Without an rlimit the
#     machine starts swapping and the test suite hangs the developer's box
#     instead of failing. With one, the child dies quickly and cleanly.
#   - A missing step guard produces a hang, caught by the timeout.
#
# The rule this harness enforces: for untrusted input, the interpreter must
# always exit with a *diagnosable error*, never a signal and never a hang.

require "json"
require "open3"
require "tempfile"
require "timeout"

module TrongoHarness
  # Exit code the CLI uses for "stopped on a resource ceiling", as opposed
  # to 1 for an ordinary script error. Must match EXIT_RESOURCE_LIMIT in
  # crates/trongoscript-cli/src/main.rs.
  EXIT_RESOURCE_LIMIT = 3
  EXIT_SCRIPT_ERROR = 1
  EXIT_OK = 0

  # How long any single stress case may run before we call it a hang. The
  # limits under test are meant to trip in well under a second; anything
  # near this bound is a failure of metering, not a slow machine.
  DEFAULT_TIMEOUT = 25

  # Address-space cap for the child, in bytes. A script that evades the
  # allocation budget dies here instead of taking the machine with it.
  # Generous enough that a legitimate run never touches it.
  DEFAULT_MEMORY_LIMIT_MB = 2048

  # Result of one CLI invocation.
  Result = Struct.new(
    :stdout, :stderr, :status, :timed_out, :duration, :cmd,
    keyword_init: true
  ) do
    # Process exited normally with code 0.
    def ok? = !timed_out && status&.exitstatus&.zero?

    # Process was killed by a signal — a crash, not an error. This is the
    # failure mode resource metering exists to prevent.
    def crashed? = !timed_out && !status.nil? && status.signaled?

    def signal_name
      return nil unless crashed?

      Signal.signame(status.termsig) || "SIG#{status.termsig}"
    end

    def exitstatus = status&.exitstatus

    # Exited on a resource ceiling, with the dedicated exit code.
    def resource_limited? = exitstatus == EXIT_RESOURCE_LIMIT

    # Exited with a normal script error.
    def script_error? = exitstatus == EXIT_SCRIPT_ERROR

    # Terminated in *some* diagnosable way: a clean exit code with a
    # message, rather than a signal or a hang.
    def diagnosable? = !timed_out && !crashed?

    def json = JSON.parse(stdout)

    # Fuel accounting parsed from `--report-fuel` output on stderr.
    def fuel
      m = stderr.match(
        /fuel: steps=(\d+) allocated=(\d+) peak_call_depth=(\d+) peak_eval_depth=(\d+)/
      )
      return nil unless m

      {
        steps: m[1].to_i,
        allocated: m[2].to_i,
        peak_call_depth: m[3].to_i,
        peak_eval_depth: m[4].to_i
      }
    end

    # Compact description used in assertion messages.
    def describe
      return "timed out after #{duration.round(1)}s" if timed_out
      return "killed by #{signal_name}" if crashed?

      "exit=#{exitstatus} stderr=#{stderr.strip.lines.first&.strip.inspect}"
    end
  end

  class BinaryMissing < StandardError; end

  module_function

  def repo_root
    @repo_root ||= File.expand_path("../..", __dir__)
  end

  # Path to the binary under test. Prefers release (stress tests want the
  # real thing — debug builds have much larger stack frames, which changes
  # how deep recursion gets before native limits matter).
  def binary
    @binary ||= begin
      candidates = [
        ENV["TRONGO_BIN"],
        File.join(repo_root, "target", "release", "trongo"),
        File.join(repo_root, "target", "debug", "trongo")
      ].compact
      found = candidates.find { |p| File.executable?(p) }
      unless found
        raise BinaryMissing,
              "no trongo binary found. Run `cargo build --release` first, " \
              "or set TRONGO_BIN. Looked in:\n  #{candidates.join("\n  ")}"
      end
      found
    end
  end

  # Run `trongo eval` on `source`, returning a Result.
  #
  # Options:
  #   limits:  hash of fuel flags, e.g. {max_steps: 1000, untrusted: true}
  #   timeout: wall-clock seconds before the child is killed
  #   memory_mb: address-space rlimit for the child
  #   subcommand: :eval (default), :render, or :embed
  #   args: extra CLI arguments
  def run_source(source, limits: {}, timeout: DEFAULT_TIMEOUT,
                 memory_mb: DEFAULT_MEMORY_LIMIT_MB, subcommand: :eval,
                 args: [])
    with_source_file(source, subcommand) do |path|
      run_cli([subcommand.to_s, "-i", path, *fuel_flags(limits), *args],
              timeout: timeout, memory_mb: memory_mb)
    end
  end

  # Run the CLI with an explicit argument list.
  def run_cli(argv, timeout: DEFAULT_TIMEOUT, memory_mb: DEFAULT_MEMORY_LIMIT_MB)
    cmd = [binary, *argv]
    started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    out_r, out_w = IO.pipe
    err_r, err_w = IO.pipe

    pid = spawn_limited(cmd, out_w, err_w, memory_mb)
    out_w.close
    err_w.close

    stdout = +""
    stderr = +""
    status = nil
    timed_out = false

    # Drain both pipes while waiting, so a child producing lots of output
    # cannot deadlock on a full pipe buffer while we wait for it to exit.
    reader = Thread.new do
      files = [out_r, err_r]
      until files.empty?
        ready, = IO.select(files, nil, nil, 0.1)
        next unless ready

        ready.each do |io|
          chunk = begin
            io.read_nonblock(65_536)
          rescue IO::WaitReadable
            next
          rescue EOFError
            files.delete(io)
            next
          end
          (io == out_r ? stdout : stderr) << chunk
        end
      end
    end

    begin
      Timeout.timeout(timeout) { _, status = Process.waitpid2(pid) }
    rescue Timeout::Error
      timed_out = true
      kill_tree(pid)
      begin
        Process.waitpid(pid)
      rescue Errno::ECHILD, Errno::ESRCH
        nil
      end
    end

    reader.join(2)
    reader.kill if reader.alive?
    [out_r, err_r].each { |io| io.close unless io.closed? }

    Result.new(
      stdout: stdout, stderr: stderr, status: status, timed_out: timed_out,
      duration: Process.clock_gettime(Process::CLOCK_MONOTONIC) - started,
      cmd: cmd
    )
  end

  # Spawn the child with an address-space rlimit so a runaway allocation
  # dies fast instead of driving the host into swap.
  def spawn_limited(cmd, out_w, err_w, memory_mb)
    bytes = memory_mb * 1024 * 1024
    opts = { out: out_w, err: err_w, pgroup: true }
    begin
      opts[:rlimit_as] = [bytes, bytes]
      Process.spawn(*cmd, **opts)
    rescue ArgumentError, Errno::EINVAL
      # Platform without RLIMIT_AS support; run without the cap. The
      # timeout still bounds the damage.
      opts.delete(:rlimit_as)
      Process.spawn(*cmd, **opts)
    end
  end

  # Kill the child's whole process group, escalating to SIGKILL.
  def kill_tree(pid)
    Process.kill("-TERM", Process.getpgid(pid))
    sleep 0.2
    Process.kill("-KILL", Process.getpgid(pid))
  rescue Errno::ESRCH, Errno::EPERM
    nil
  end

  # Translate a limits hash into CLI flags.
  def fuel_flags(limits)
    flags = []
    flags << "--untrusted" if limits[:untrusted]
    flags << "--no-limits" if limits[:no_limits]
    flags << "--report-fuel" if limits.fetch(:report_fuel, true)
    { max_steps: "--max-steps", max_call_depth: "--max-call-depth",
      max_eval_depth: "--max-eval-depth", max_value_len: "--max-value-len",
      max_alloc: "--max-alloc" }.each do |key, flag|
      flags += [flag, limits[key].to_s] if limits[key]
    end
    flags
  end

  # Write `source` to a temp file with the extension the subcommand wants.
  def with_source_file(source, subcommand)
    ext = subcommand == :embed ? ".json" : ".trongo"
    file = Tempfile.new(["stress", ext])
    file.write(source)
    file.flush
    yield file.path
  ensure
    file&.close
    file&.unlink
  end
end

# Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
