using Regain.Core;
using Xunit;

namespace Regain.Tests;

public sealed class MountRecoveryJournalTests
{
    [Fact] public void FailedAttemptSurvivesNewInstanceAndCannotBeClearedWhileOperationIsActive()
    {
        string directory = Path.Combine(Path.GetTempPath(), "regain-mount-" + Guid.NewGuid());
        var journal = new MountRecoveryJournal(directory);
        const string id = "Regain.Mount/ASCOM.Simulator.Telescope";
        try {
            using (var attempt = journal.Begin(id)) {
                Assert.True(journal.Required(id)); Assert.False(journal.ClearAfterInspection(id));
                Assert.Throws<IOException>(() => journal.Begin(id));
            }
            var restarted = new MountRecoveryJournal(directory);
            Assert.True(restarted.Required(id)); Assert.Throws<IOException>(() => restarted.Begin(id));
            Assert.True(restarted.ClearAfterInspection(id)); Assert.False(restarted.Required(id));
            using (var success = restarted.Begin(id)) success.Complete();
            Assert.False(journal.Required(id));
        } finally { Directory.Delete(directory, recursive: true); }
    }
}
