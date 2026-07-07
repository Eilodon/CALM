using MultiLang;

class Program
{
    // Resolves Helper.Greet via the `using MultiLang;` namespace import, not
    // a same-file/same-dir match — the gap P1.5's namespace->files table closes.
    static void Main()
    {
        System.Console.WriteLine(Helper.Greet("world"));
    }
}
