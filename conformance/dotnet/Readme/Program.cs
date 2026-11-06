// Runs the example of clients/dotnet/README.md against the runner's driver; run.sh calls it
// after Quickstart, so the README's program is a test (CLAUDE.md, "Clients and developer
// experience"; .claude/rules/docs.md, "Code in docs"). The README's program is
// clients/dotnet/example/Program.cs: this checks that the README's C# block is that file,
// byte for byte, builds the example with this SDK as the README's `dotnet run` does, runs
// the built program with this runtime for up to 10 s, and exits 1 unless it exits 0 and
// prints the line the README promises after "You will see". The README's shell lines
// around the program, the builds and the driver, are run.sh's own steps. Run from the
// checkout's root. .NET's standard library only (ADR 0003).

using System;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.RegularExpressions;

public static class Readme
{
    const string README = "clients/dotnet/README.md";
    const string EXAMPLE = "clients/dotnet/example/Program.cs";
    const string PROJECT = "clients/dotnet/example/Example.csproj";
    const string BUILT = "clients/dotnet/example/bin/Release/net10.0/Example.dll";
    const int WAIT_MILLIS = 10_000;

    static void Fail(string what)
    {
        Console.Error.WriteLine("conformance/dotnet: " + README + "'s example: " + what);
        Environment.Exit(1);
    }

    static string Quote(string s)
    {
        return "\"" + s.Replace("\\", "\\\\").Replace("\"", "\\\"").Replace("\n", "\\n") + "\"";
    }

    public static void Main()
    {
        string text = File.ReadAllText(README, Encoding.UTF8);
        Match program = Regex.Match(text, "```csharp\n(.*?)```\n", RegexOptions.Singleline);
        Match expected = Regex.Match(text, "^You will see `([^`]+)`\\.$", RegexOptions.Multiline);
        if (!program.Success || !expected.Success)
        {
            Fail("no ```csharp block, or no line \"You will see `...`.\" after it; keep both shapes so this test can find them");
        }
        string example = File.ReadAllText(EXAMPLE, Encoding.UTF8);
        if (program.Groups[1].Value != example)
        {
            Fail("the README's C# block is not " + EXAMPLE + "; the README shows the program that runs, so change both");
        }
        string dotnet = Path.Combine(Path.GetDirectoryName(Environment.ProcessPath) ?? "", "dotnet");
        if (!File.Exists(dotnet))
        {
            dotnet = "dotnet";
        }
        Process build = Process.Start(new ProcessStartInfo(dotnet, new[] { "build", "-c", "Release", PROJECT })
        {
            RedirectStandardOutput = true, RedirectStandardError = true, UseShellExecute = false,
        })!;
        string buildOut = build.StandardOutput.ReadToEnd();
        string buildErr = build.StandardError.ReadToEnd();
        build.WaitForExit();
        if (build.ExitCode != 0)
        {
            Fail("dotnet build of " + PROJECT + " failed with " + build.ExitCode + "; it printed " + Quote(buildOut) + " and " + Quote(buildErr));
        }
        Process run = Process.Start(new ProcessStartInfo(dotnet, new[] { BUILT })
        {
            RedirectStandardOutput = true, RedirectStandardError = true, UseShellExecute = false,
        })!;
        string stdout = "", stderr = "";
        System.Threading.Tasks.Task<string> readOut = run.StandardOutput.ReadToEndAsync();
        System.Threading.Tasks.Task<string> readErr = run.StandardError.ReadToEndAsync();
        if (!run.WaitForExit(WAIT_MILLIS))
        {
            run.Kill(true);
            run.WaitForExit();
            Fail("did not finish in " + WAIT_MILLIS / 1000 + " s; it printed " + Quote(readOut.Result));
        }
        stdout = readOut.Result;
        stderr = readErr.Result;
        if (run.ExitCode != 0)
        {
            Fail("exited with " + run.ExitCode + "; it printed " + Quote(stdout) + " and " + Quote(stderr));
        }
        string line = expected.Groups[1].Value;
        foreach (string printed in stdout.Split('\n'))
        {
            if (printed == line)
            {
                Console.WriteLine("conformance/dotnet: " + README + "'s example printed `" + line + "`, as the README says");
                return;
            }
        }
        Fail("printed " + Quote(stdout) + ", not the line the README says the reader will see, " + Quote(line));
    }
}
