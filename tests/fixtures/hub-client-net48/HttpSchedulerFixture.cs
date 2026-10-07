using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using Regain.TestFixtures;

// Run only in this private child process: never change the test runner's pool.
internal static class HttpSchedulerFixture
{
    internal static void Run()
    {
        ThreadPool.GetMinThreads(out var oldMinimum, out var oldMinimumIo);
        ThreadPool.GetMaxThreads(out var oldMaximum, out var oldMaximumIo);
        var count = Environment.ProcessorCount;
        using var occupied = new CountdownEvent(count);
        using var release = new ManualResetEventSlim();
        using var finished = new CountdownEvent(count);
        HubCoverCalibratorServer? server = null;
        if (!ThreadPool.SetMinThreads(count, oldMinimumIo) ||
            !ThreadPool.SetMaxThreads(count, oldMaximumIo))
            throw new InvalidOperationException("Could not isolate the fixture worker pool");
        try {
            for (var index = 0; index < count; index++) ThreadPool.QueueUserWorkItem(_ => {
                occupied.Signal(); release.Wait(); finished.Signal();
            });
            if (!occupied.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException("Pool saturation did not start");
            ThreadPool.GetAvailableThreads(out var available, out _);
            if (available != 0) throw new InvalidOperationException("Shared pool was not fully occupied");
            server = new HubCoverCalibratorServer();
            server.LoseMoveReply = false;
            var endpoint = new Uri(server.Url);
            for (var index = 1; index <= 8; index++) {
                using var client = new TcpClient { ReceiveTimeout = 3000, SendTimeout = 3000, NoDelay = true };
                client.Connect(endpoint.Host, endpoint.Port);
                using var stream = client.GetStream();
                var request = Encoding.ASCII.GetBytes("GET /api/v1/covercalibrator/19/maxbrightness?ClientID=1&ClientTransactionID=" + index +
                    " HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
                stream.Write(request, 0, request.Length);
                using var reader = new StreamReader(stream, Encoding.UTF8);
                if (reader.ReadLine() != "HTTP/1.1 200 OK") throw new IOException("Missing HTTP success");
                while (reader.ReadLine() is string line && line.Length != 0) { }
                using var response = JsonDocument.Parse(reader.ReadToEnd());
                if (response.RootElement.GetProperty("ErrorNumber").GetInt32() != 0 ||
                    response.RootElement.GetProperty("Value").GetInt32() != 4096)
                    throw new InvalidOperationException("Malformed fixture response under shared-pool saturation");
            }
            // A partial request must also drain during disposal without a pool
            // continuation or waiting for its sender to complete the headers.
            var accepted = server.AcceptedClients;
            using var unfinished = new TcpClient();
            unfinished.Connect(endpoint.Host, endpoint.Port);
            unfinished.GetStream().WriteByte((byte)'G');
            if (!SpinWait.SpinUntil(() => server.AcceptedClients > accepted, TimeSpan.FromSeconds(3)))
                throw new TimeoutException("The partial request was not accepted");
            server.Dispose();
            Console.WriteLine($"HTTP fixture {IntPtr.Size * 8}-bit: eight replies and partial-client cleanup with every shared worker occupied passed");
        } finally {
            release.Set();
            if (!finished.Wait(TimeSpan.FromSeconds(10))) throw new TimeoutException("Pool saturation did not drain");
            ThreadPool.SetMaxThreads(oldMaximum, oldMaximumIo);
            ThreadPool.SetMinThreads(oldMinimum, oldMinimumIo);
            server?.Dispose();
        }
    }
}
