use std::collections::HashMap;

pub fn assignment_nodes() -> HashMap<&'static str, &'static [&'static str]> {
    let mut m = HashMap::new();
    m.insert("python", ["assignment", "augmented_assignment"].as_slice());
    m.insert("typescript", ["variable_declarator"].as_slice());
    m.insert("javascript", ["variable_declarator"].as_slice());
    m.insert("java", ["local_variable_declaration"].as_slice());
    m.insert("rust", ["let_declaration"].as_slice());
    m.insert(
        "go",
        ["short_var_declaration", "var_declaration"].as_slice(),
    );
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_languages_present() {
        let nodes = assignment_nodes();
        assert!(nodes.contains_key("python"));
        assert!(nodes.contains_key("typescript"));
        assert!(nodes.contains_key("javascript"));
        assert!(nodes.contains_key("java"));
        assert!(nodes.contains_key("rust"));
        assert!(nodes.contains_key("go"));
        assert_eq!(nodes.len(), 6);
    }

    #[test]
    fn test_python_nodes() {
        let nodes = assignment_nodes();
        let py = nodes["python"];
        assert!(py.contains(&"assignment"));
        assert!(py.contains(&"augmented_assignment"));
    }
}
