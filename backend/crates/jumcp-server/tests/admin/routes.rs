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
    assert!(table.iter().any(|(_, path)| *path == "/admin/mcp/catalog"));
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/transport-key")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/mcp/catalog/{id}/install")
    );
    assert!(
        table
            .iter()
            .any(|(_, path)| *path == "/admin/mcp/catalog/{id}/install/{upstream_id}")
    );
    assert!(table.iter().any(|(method, path)| *method == "PATCH"
        && *path == "/admin/mcp/catalog/{id}/install/{upstream_id}/config"));
    // 登录契约：三条路由必须在文档清单里，否则运维照着清单核对会漏掉登录面。
    assert!(
        table
            .iter()
            .any(|(method, path)| *method == "POST" && *path == "/admin/login")
    );
    assert!(
        table
            .iter()
            .any(|(method, path)| *method == "POST" && *path == "/admin/logout")
    );
    assert!(
        table
            .iter()
            .any(|(method, path)| *method == "GET" && *path == "/admin/session")
    );
    assert_eq!(table.len(), 43);
}
