import java.util.ArrayList;
import java.util.List;

/** Creates an empty ArrayList with a capacity of 1,000,000 and waits forever */
public class EmptyArrayList {

    public static void main(String[] args) throws InterruptedException {
        List<String> list = new ArrayList<>(1000000);
        Thread.sleep(Integer.MAX_VALUE);
    }
}
