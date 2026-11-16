#[tokio::test]
async fn routes_are_documented() {
    let table = jumcp_server::admin::routes();
    assert!(table.iter().any(|(_, path)| *path == "/admin/review-queue"));
    assert!(table.iter().any(|(_, path)| *path == "/health"));
    assert!(table.iter().any(|(_, path)| *path == "/admin/capabilities"));
    assert!(table.iter().any(|(_, path)| *path == "/admin/tools/stats"));
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/tools/{id}/calls")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/capabilities/{id}/deprecate")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/capabilities/{id}/reject")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/capabilities/{id}/definition")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/capabilities/{id}/versions")
    );
    assert!(table.iter().any(|(_, path)| *path == "/admin/me"));
    assert!(table.iter().any(|(_, path)| *path == "/admin/roles"));
    assert!(table.iter().any(|(_, path)| *path == "/admin/groups"));
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/groups/{code}")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/upstreams/{id}/tools")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/upstreams/{id}/tools/basic")
    );
    assert_eq!(table.len(), 35);
}
