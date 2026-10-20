// Compound predicates (`&&`, `||`) and the conditional operator.
package fixtures;

final class Predicates {
    static boolean canPublish(boolean approved, boolean drafted) {
        return approved && drafted;
    }

    static boolean shouldRetry(int attempts, boolean fatal) {
        return attempts < 3 || !fatal;
    }

    static boolean mixed(boolean a, boolean b, boolean c) {
        return a && b || c;
    }

    static String label(int count) {
        return count > 0 ? "some" : "none";
    }
}
