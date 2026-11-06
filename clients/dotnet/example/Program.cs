using System.Buffers.Binary;
using Cavalry;

int[] received = new int[1000];
int count = 0;
using Client client = new(); // the driver at CAVALRY_DIR, or its default
Subscription subscription = client.AddSubscription("cavalry:ipc", 1001);
Publication publication = client.AddPublication("cavalry:ipc", 1001);
FragmentHandler onFragment = (body, header) => // body: a span over the log, for the call
    received[count++] = BinaryPrimitives.ReadInt32LittleEndian(body);
for (int sent = 0; sent < 1000;)
{
    if (publication.TryClaim(32) < 0) // the position after the frame, or a code below 0
    {
        subscription.Poll(onFragment, 10); // back-pressured or not yet connected: drain, retry
        continue;
    }
    BinaryPrimitives.WriteInt32LittleEndian(publication.Claim, sent); // write into the log
    publication.Commit();
    sent++;
}
while (count < 1000)
{
    subscription.Poll(onFragment, 10);
}
for (int i = 0; i < 1000; i++)
{
    if (received[i] != i)
    {
        throw new InvalidOperationException("every message once, in order");
    }
}
Console.WriteLine($"{count} messages over cavalry:ipc, position {publication.Position()}");
