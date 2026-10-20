// Compound predicates (`&&`, `||`) and the conditional operator.
namespace Fixtures
{
    static class Predicates
    {
        public static bool CanPublish(bool approved, bool drafted)
        {
            return approved && drafted;
        }

        public static bool ShouldRetry(int attempts, bool fatal)
        {
            return attempts < 3 || !fatal;
        }

        public static bool Mixed(bool a, bool b, bool c)
        {
            return a && b || c;
        }

        public static string Label(int count)
        {
            return count > 0 ? "some" : "none";
        }
    }
}
