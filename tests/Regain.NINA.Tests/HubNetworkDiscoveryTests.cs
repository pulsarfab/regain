using System.Text.Json;
using System.Text.Json.Nodes;
using Regain.Hub;
using Xunit;

namespace Regain.NINA.Tests;

public sealed class HubNetworkDiscoveryTests
{
    private static JsonElement Reply(Guid revision) => JsonSerializer.SerializeToElement(new {
        configurationRevision = revision, interfacesTried = 4, interfacesFailed = 1, ignoredDatagrams = 2, incomplete = true,
        servers = new object[] {
            new { address = "192.0.2.3", scopeId = 0, port = 80, baseUrl = "http://192.0.2.3:80" },
            new { address = "2001:db8::42", scopeId = 0, port = 11111, baseUrl = "http://[2001:db8::42]:11111" },
            new { address = "fe80::42", scopeId = 7, port = 11111, baseUrl = "http://[fe80::42]:11111" },
            new { address = "fe80::42", scopeId = 8, port = 11111, baseUrl = "http://[fe80::42]:11111" },
            new { address = "::c000:203", scopeId = 0, port = 1, baseUrl = "http://[::c000:203]:1" }
        }
    });
    private static HubEditorSession Editor(Func<JsonElement, CancellationToken, Task<JsonElement>> search)
    {
        var saved = HubDraftTests.Configuration(); var revision = saved.GetProperty("revision").GetGuid();
        return new(saved.GetProperty("instanceId").GetGuid(), (command, token) => command.GetProperty("op").GetString() switch {
            "describeConfig" => Task.FromResult(HubDraftTests.Description()), "getConfig" => Task.FromResult(saved),
            "hostStatus" => Task.FromResult(JsonSerializer.SerializeToElement(new { phase = "ready", configurationRevision = revision })),
            "validateConfig" => Task.FromResult(JsonSerializer.SerializeToElement(new { valid = true, errors = Array.Empty<object>() })),
            "searchAlpaca" => search(command, token), _ => throw new Exception("Unexpected catalog, credential, configuration or equipment I/O")
        }, () => { });
    }
    [Fact]
    public async Task SearchPreservesReviewedDraftAndScopedCandidatesWithoutOtherIo()
    {
        var calls = 0;
        using var editor = Editor((command, _) => {
            Assert.Equal(new[] { "op", "expectedRevision" }, command.EnumerateObject().Select(p => p.Name)); calls++;
            return Task.FromResult(Reply(command.GetProperty("expectedRevision").GetGuid()));
        });
        await editor.ReloadAsync(); Assert.True(await editor.ReviewAsync()); var before = editor.Draft!.Candidate.GetRawText();
        var result = await editor.SearchAlpacaAsync();
        Assert.Equal(5, result.GetProperty("servers").GetArrayLength());
        Assert.Equal(7u, result.GetProperty("servers")[2].GetProperty("scopeId").GetUInt32());
        Assert.Equal(8u, result.GetProperty("servers")[3].GetProperty("scopeId").GetUInt32());
        Assert.Equal("http://[fe80::42]:11111", result.GetProperty("servers")[2].GetProperty("baseUrl").GetString());
        Assert.Equal(before, editor.Draft.Candidate.GetRawText()); Assert.False(editor.Draft.Dirty);
        Assert.Equal(HubEditorState.Reviewed, editor.State); Assert.Null(editor.LastDiscovery); Assert.Equal(1, calls);
        await editor.ReloadAsync(); Assert.Null(editor.LastNetworkDiscovery);
    }
    [Theory]
    [InlineData("revision")][InlineData("port")][InlineData("address")][InlineData("scope")]
    [InlineData("url")][InlineData("reason")][InlineData("duplicate")][InlineData("extra")]
    [InlineData("counts")][InlineData("incomplete")][InlineData("limit")][InlineData("mapped")]
    [InlineData("multicast")][InlineData("lost")]
    public async Task MalformedOrLostSearchRequiresReloadAndNeverRetries(string fault)
    {
        var calls = 0;
        using var editor = Editor((command, _) => {
            calls++; if (fault == "lost") throw new HubException(HubFailure.Disconnected);
            var reply = JsonNode.Parse(Reply(command.GetProperty("expectedRevision").GetGuid()).GetRawText())!;
            var server = reply["servers"]![0]!;
            if (fault == "revision") reply["configurationRevision"] = Guid.NewGuid();
            if (fault == "port") server["port"] = 0;
            if (fault == "address") server["address"] = "127.1";
            if (fault == "scope") reply["servers"]![2]!["scopeId"] = 0;
            if (fault == "url") reply["servers"]![2]!["baseUrl"] = "http://[fe80::43]:11111";
            if (fault == "reason") reply["servers"]![2]!["unavailableReason"] = null;
            if (fault == "duplicate") reply["servers"]!.AsArray().Add(server.DeepClone());
            if (fault == "extra") reply["authorization"] = "must-not-escape";
            if (fault == "counts") reply["interfacesFailed"] = 5;
            if (fault == "incomplete") reply["incomplete"] = false;
            if (fault == "limit") reply["servers"] = new JsonArray(Enumerable.Range(0, 257).Select(_ => server.DeepClone()).ToArray());
            if (fault == "mapped") { server["address"] = "::ffff:c000:203"; server["baseUrl"] = "http://[::ffff:c000:203]:80"; }
            if (fault == "multicast") { server["address"] = "ff12::42"; server["baseUrl"] = "http://[ff12::42]:80"; }
            return Task.FromResult(JsonSerializer.SerializeToElement(reply));
        });
        await editor.ReloadAsync(); await editor.ReviewAsync();
        var error = await Assert.ThrowsAsync<HubException>(() => editor.SearchAlpacaAsync());
        Assert.DoesNotContain("must-not-escape", error.Message); Assert.Null(editor.LastNetworkDiscovery);
        Assert.Equal(HubEditorState.Uncertain, editor.State); Assert.False(editor.Draft!.Dirty);
        await Assert.ThrowsAsync<InvalidOperationException>(() => editor.SearchAlpacaAsync()); Assert.Equal(1, calls);
    }
    [Fact]
    public async Task CancelledSearchInvalidatesReviewWithoutRetryOrRetainedCandidates()
    {
        using var cancellation = new CancellationTokenSource(); var calls = 0;
        using var editor = Editor((_, token) => { calls++; cancellation.Cancel(); return Task.FromCanceled<JsonElement>(token); });
        await editor.ReloadAsync(); await editor.ReviewAsync();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => editor.SearchAlpacaAsync(cancellation.Token));
        Assert.Equal(HubEditorState.Uncertain, editor.State); Assert.Null(editor.LastNetworkDiscovery); Assert.Equal(1, calls);
    }
}
