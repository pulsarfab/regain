using System.Buffers.Binary;
using System.Diagnostics;
using System.Text.Json;

namespace ZwoGain.Core;

public sealed class SdkException(string message, int? code = null, string? operation = null) : IOException(message)
{
    public int? Code { get; } = code;
    public string? Operation { get; } = operation;
    public bool Retryable => Code is null or 1 or 2 or 4 or 5 or 11 or 12 or 15 or 16;
}
public sealed record Reply(JsonElement Result, byte[] Pixels);

/// <summary>Private inherited anonymous pipes: bounded JSON followed by contiguous RAW16 bytes.</summary>
public sealed class HostClient : IDisposable
{
    private readonly Process process;
    private readonly ProcessJob? job;
    private readonly SemaphoreSlim gate = new(1);
    private long nextId;
    private int disposed;
    public int ProcessId => process.Id;
    public HostClient(string executable, string sdk, bool simulate = false, Action<string>? log = null)
    {
        var start = new ProcessStartInfo(Path.GetFullPath(executable))
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            WorkingDirectory = Path.GetDirectoryName(Path.GetFullPath(executable))!
        };
        if (simulate)
            start.ArgumentList.Add("--simulate");
        else
        {
            start.ArgumentList.Add("--sdk");
            start.ArgumentList.Add(Path.GetFullPath(sdk));
        }
        process = Process.Start(start) ?? throw new IOException("Camera host failed to start");
        try
        {
            job = ProcessJob.Attach(process);
        }
        catch { process.Kill(true); process.Dispose(); throw; }
        process.ErrorDataReceived += (_, e) => { if (e.Data is not null) log?.Invoke(e.Data); };
        process.BeginErrorReadLine();
    }
    public async Task<Reply> CallAsync(string method, object? args, TimeSpan timeout, CancellationToken token)
    {
        await gate.WaitAsync(token).ConfigureAwait(false);
        try
        {
            ObjectDisposedException.ThrowIf(disposed != 0, this);
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(token);
            deadline.CancelAfter(timeout);
            try
            {
                long id = ++nextId;
                var request = JsonSerializer.SerializeToUtf8Bytes(new
                {
                    version = 1,
                    id,
                    method,
                    @params = args
                });
                if (request.Length > 65536)
                    throw new InvalidDataException("Request too large");
                byte[] length = new byte[4];
                BinaryPrimitives.WriteInt32LittleEndian(length, request.Length);
                var input = process.StandardInput.BaseStream;
                var output = process.StandardOutput.BaseStream;
                await input.WriteAsync(length, deadline.Token).ConfigureAwait(false);
                await input.WriteAsync(request, deadline.Token).ConfigureAwait(false);
                await input.FlushAsync(deadline.Token).ConfigureAwait(false);
                await output.ReadExactlyAsync(length, deadline.Token).ConfigureAwait(false);
                int count = BinaryPrimitives.ReadInt32LittleEndian(length);
                if (count is <= 0 or > 65536)
                    throw new InvalidDataException("Invalid host JSON length");
                byte[] json = new byte[count];
                await output.ReadExactlyAsync(json, deadline.Token).ConfigureAwait(false);
                using var doc = JsonDocument.Parse(json);
                var root = doc.RootElement;
                if (root.GetProperty("version").GetInt32() != 1 || root.GetProperty("id").GetInt64() != id)
                    throw new InvalidDataException("Stale or incompatible host response");
                int n = root.GetProperty("binaryLength").GetInt32();
                if (n is < 0 or > 536870912 || (method != "download" && n != 0))
                    throw new InvalidDataException("Invalid frame length");
                if (!root.GetProperty("ok").GetBoolean())
                {
                    if (n != 0)
                        throw new InvalidDataException("Error response contains pixels");
                    int? sdkCode = root.TryGetProperty("sdkCode", out var code) && code.ValueKind == JsonValueKind.Number ? code.GetInt32() : null;
                    string? sdkOperation = root.TryGetProperty("sdkOperation", out var op) && op.ValueKind == JsonValueKind.String ? op.GetString() : null;
                    throw new SdkException(root.GetProperty("error").GetString()!, sdkCode, sdkOperation);
                }
                var result = root.GetProperty("result").Clone();
                if (method == "download" && (n == 0 || n != checked(result.GetProperty("width").GetInt32() * result.GetProperty("height").GetInt32() * 2)))
                    throw new InvalidDataException("Frame dimensions do not match payload");
                byte[] pixels = new byte[n];
                await output.ReadExactlyAsync(pixels, deadline.Token).ConfigureAwait(false);
                return new(result, pixels);
            }
            catch (OperationCanceledException)
            {
                Dispose();
                token.ThrowIfCancellationRequested();
                throw new TimeoutException($"Camera host timed out during {method}");
            }
            catch (SdkException) { throw; }
            catch { Dispose(); throw; }
        }
        finally { gate.Release(); }
    }
    public void Dispose()
    {
        if (Interlocked.Exchange(ref disposed, 1) != 0)
            return;
        try
        {
            if (!process.HasExited)
            {
                process.Kill(entireProcessTree: true);
                process.WaitForExit(3000);
            }
        }
        catch (InvalidOperationException) { }
        finally { job?.Dispose(); process.Dispose(); }
    }
}
