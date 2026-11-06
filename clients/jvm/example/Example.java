import com.appshapes.cavalry.Client;
import com.appshapes.cavalry.FragmentHandler;
import com.appshapes.cavalry.Publication;
import com.appshapes.cavalry.Subscription;

import static java.lang.foreign.ValueLayout.JAVA_INT_UNALIGNED;

public class Example {
    public static void main(String[] args) {
        int[] received = new int[1000];
        int[] count = {0};
        try (Client client = new Client()) { // the driver at CAVALRY_DIR, or its default
            Subscription subscription = client.addSubscription("cavalry:ipc", 1001);
            Publication publication = client.addPublication("cavalry:ipc", 1001);
            FragmentHandler onFragment = (body, header) -> // body: a view into the log, for the call
                received[count[0]++] = body.get(JAVA_INT_UNALIGNED, 0);
            int sent = 0;
            while (sent < 1000) {
                if (publication.tryClaim(32) < 0) { // the position after the frame, or a code below 0
                    subscription.poll(onFragment, 10); // back-pressured or not yet connected: drain, retry
                    continue;
                }
                publication.claim().set(JAVA_INT_UNALIGNED, 0, sent); // write into the log
                publication.commit();
                sent++;
            }
            while (count[0] < 1000) {
                subscription.poll(onFragment, 10);
            }
            for (int i = 0; i < 1000; i++) {
                if (received[i] != i) {
                    throw new IllegalStateException("every message once, in order");
                }
            }
            System.out.println(count[0] + " messages over cavalry:ipc, position " + publication.position());
        }
    }
}
