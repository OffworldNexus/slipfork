//! `ForkFile` lookups and the recursive statement walker.

use crate::syntax::*;

/// Visit every statement, including the bodies of `on` blocks, in source order.
pub(crate) fn each_stmt<'a>(statements: &'a [Stmt], visit: &mut dyn FnMut(&'a Stmt)) {
    for statement in statements {
        visit(statement);
        if let Stmt::On(block) = statement {
            each_stmt(&block.body, visit);
        }
    }
}
impl ForkFile {
    /// `true` when `id` names any declared entity (project, feature, resource, service, task).
    pub fn has_id(&self, id: &str) -> bool {
        self.entity_kind(id).is_some()
    }

    /// Find a declared service by id.
    pub fn find_service(&self, id: &str) -> Option<&ServiceDef> {
        let mut found = None;
        each_stmt(&self.statements, &mut |statement| {
            if found.is_some() {
                return;
            }
            if let Stmt::Service(service) = statement
                && service.id.value == id
            {
                found = Some(service);
            }
        });
        found
    }

    /// Find a service's container source by service id.
    pub fn find_image(&self, id: &str) -> Option<&ImageSource> {
        self.find_service(id).map(|service| &service.source)
    }

    /// Find a postgres cluster by id.
    pub fn find_pg_cluster(&self, id: &str) -> Option<&PgClusterDef> {
        let mut found = None;
        each_stmt(&self.statements, &mut |statement| {
            if found.is_some() {
                return;
            }
            if let Stmt::Postgres(cluster) = statement
                && cluster.id.value == id
            {
                found = Some(cluster);
            }
        });
        found
    }

    /// Find a postgres database by id, together with its owning cluster.
    pub fn find_pg_database(&self, id: &str) -> Option<(&PgClusterDef, &PgDatabaseDef)> {
        let mut found = None;
        each_stmt(&self.statements, &mut |statement| {
            if found.is_some() {
                return;
            }
            let Stmt::Postgres(cluster) = statement else {
                return;
            };
            if let Some(database) = cluster
                .databases
                .iter()
                .find(|database| database.id.value == id)
            {
                found = Some((cluster, database));
                return;
            }
            for block in &cluster.on {
                if let Some(database) = block
                    .databases
                    .iter()
                    .find(|database| database.id.value == id)
                {
                    found = Some((cluster, database));
                    return;
                }
            }
        });
        found
    }

    /// Find a redis cluster by id.
    pub fn find_redis_cluster(&self, id: &str) -> Option<&RedisClusterDef> {
        let mut found = None;
        each_stmt(&self.statements, &mut |statement| {
            if found.is_some() {
                return;
            }
            if let Stmt::Redis(cluster) = statement
                && cluster.id.value == id
            {
                found = Some(cluster);
            }
        });
        found
    }
}

impl ForkFile {
    /// The kind of entity `id` names, or `None` when it is not declared.
    pub fn entity_kind(&self, id: &str) -> Option<&'static str> {
        let mut found = None;
        each_stmt(&self.statements, &mut |statement| {
            if found.is_some() {
                return;
            }
            found = match statement {
                Stmt::Project(value) if value.value == id => Some("project"),
                Stmt::Features { definitions, .. }
                    if definitions
                        .iter()
                        .any(|definition| definition.name.value == id) =>
                {
                    Some("feature")
                }
                Stmt::Postgres(cluster)
                    if cluster.id.value == id
                        || cluster
                            .databases
                            .iter()
                            .any(|database| database.id.value == id)
                        || cluster.on.iter().any(|block| {
                            block
                                .databases
                                .iter()
                                .any(|database| database.id.value == id)
                        }) =>
                {
                    Some(if cluster.id.value == id {
                        "postgres cluster"
                    } else {
                        "postgres database"
                    })
                }
                Stmt::Redis(cluster) if cluster.id.value == id => Some("redis cluster"),
                Stmt::S3(bucket) if bucket.id.value == id => Some("s3 bucket"),
                Stmt::Domain(domain) if domain.id.value == id => Some("domain"),
                Stmt::EnvGroup(group) if group.name.value == id => Some("env group"),
                Stmt::Service(service) if service.id.value == id => Some("service"),
                Stmt::Task(task) if task.name.value == id => Some("task"),
                _ => None,
            };
        });
        found
    }
}
