import java.util.HashMap;

public class Simple {

    private HashMap<String, String> map = new HashMap<>();

    public Simple() {
        for (int i = 0; i < 100; i++) {
            map.put("key" + i, "value" + i);
        }
    }

    static void main(String[] args) throws InterruptedException {
        new Simple();
        Thread.sleep(Integer.MAX_VALUE);
    }

}
